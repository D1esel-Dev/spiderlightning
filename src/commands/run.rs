use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

use anyhow::{bail, Result};
use as_any::Downcast;
use slight_common::{BasicState, Capability, WasmtimeBuildable};
use slight_core::slightfile::{Capability as TomlCapability, TomlFile};
#[cfg(feature = "distributed-locking")]
use slight_distributed_locking::DistributedLocking;
#[cfg(feature = "http-client")]
use slight_http_client::HttpClient;
#[cfg(feature = "http-server")]
use slight_http_server::HttpServer;
#[cfg(feature = "keyvalue")]
use slight_keyvalue::Keyvalue;
#[cfg(feature = "messaging")]
use slight_messaging::Messaging;
use slight_runtime::{Builder, Ctx};
#[cfg(feature = "runtime-configs")]
use slight_runtime_configs::Configs;
#[cfg(feature = "sql")]
use slight_sql::Sql;
use wit_bindgen_wasmtime::wasmtime::Store;

#[cfg(feature = "keyvalue")]
const KEYVALUE_HOST_IMPLEMENTORS: [&str; 8] = [
    "kv.filesystem",
    "kv.azblob",
    "kv.awsdynamodb",
    "kv.redis",
    "keyvalue.filesystem",
    "keyvalue.azblob",
    "keyvalue.awsdynamodb",
    "keyvalue.redis",
];

#[cfg(feature = "distributed-locking")]
const DISTRIBUTED_LOCKING_HOST_IMPLEMENTORS: [&str; 2] = ["lockd.etcd", "distributed_locking.etcd"];

#[cfg(feature = "messaging")]
const MESSAGING_HOST_IMPLEMENTORS: [&str; 8] = [
    "pubsub.confluent_apache_kafka",
    "pubsub.mosquitto",
    "messaging.confluent_apache_kafka",
    "messaging.mosquitto",
    "mq.azsbus",
    "mq.filesystem",
    "messaging.azsbus",
    "messaging.filesystem",
];

#[cfg(feature = "runtime-configs")]
const CONFIGS_HOST_IMPLEMENTORS: [&str; 3] =
    ["configs.usersecrets", "configs.envvars", "configs.azapp"];

#[cfg(feature = "sql")]
const SQL_HOST_IMPLEMENTORS: [&str; 1] = ["sql.postgres"];

pub async fn handle_run(module: impl AsRef<Path>, toml_file_path: impl AsRef<Path>) -> Result<()> {
    let toml_file_contents =
        std::fs::read_to_string(&toml_file_path).expect("could not locate slightfile");
    let toml =
        toml::from_str::<TomlFile>(&toml_file_contents).expect("provided file is not a slightfile");

    tracing::info!("Starting slight");

    let host_builder = build_store_instance(&toml, &toml_file_path, &module).await?;
    let (mut store, instance) = host_builder.build().await;

    let caps = toml.capability.as_ref().unwrap();

    // looking for the http capability.
    let http_enabled = if cfg!(feature = "http-server") {
        let http_enabled;

        if toml.specversion == "0.1" {
            http_enabled = caps.iter().any(|cap| cap.name == "http");
        } else if toml.specversion == "0.2" {
            http_enabled = caps
                .iter()
                .any(|cap| cap.resource.as_ref().expect("missing resource field") == "http");
        } else {
            bail!("unsupported toml spec version");
        }

        if http_enabled {
            log::debug!("Http capability enabled");
            update_http_states(toml, toml_file_path, module, &mut store).await?;
        }
        http_enabled
    } else {
        false
    };

    instance
        .get_typed_func::<(), _, _>(&mut store, "_start")?
        .call_async(&mut store, ())
        .await?;

    if cfg!(feature = "http-server") && http_enabled {
        log::info!("waiting for http to finish...");
        close_http_server(store).await;
    }
    Ok(())
}

#[cfg(not(feature = "http-server"))]
async fn update_http_states(
    _toml: TomlFile,
    _toml_file_path: impl AsRef<Path>,
    _module: impl AsRef<Path>,
    _store: &mut Store<slight_runtime::RuntimeContext>,
) -> Result<(), anyhow::Error> {
    log::debug!("http-server feature is not enabled");
    Ok(())
}

#[cfg(feature = "http-server")]
async fn update_http_states(
    toml: TomlFile,
    toml_file_path: impl AsRef<Path>,
    module: impl AsRef<Path>,
    store: &mut Store<slight_runtime::RuntimeContext>,
) -> Result<(), anyhow::Error> {
    let guest_builder: Builder = build_store_instance(&toml, &toml_file_path, &module).await?;
    let http_api_resource: &mut HttpServer<Builder> = get_resource(store, "http");
    http_api_resource.update_state(slight_common::Builder::new(guest_builder))?;
    Ok(())
}

#[cfg(not(feature = "http-server"))]
async fn close_http_server(_store: Store<slight_runtime::RuntimeContext>) {
    log::debug!("http-server feature is not enabled");
}

#[cfg(feature = "http-server")]
async fn close_http_server(mut store: Store<slight_runtime::RuntimeContext>) {
    shutdown_signal().await;
    let http_api_resource: &mut HttpServer<Builder> = get_resource(&mut store, "http");
    http_api_resource.close();
}

fn get_resource<'a, T>(store: &'a mut Store<Ctx>, scheme_name: &'a str) -> &'a mut T
where
    T: Capability,
{
    let err_msg = format!("internal error: slight context does not contain key: {scheme_name}");
    let err_msg2 =
        format!("internal error: slight context contains key {scheme_name} but can't downcast");
    store
        .data_mut()
        .slight
        .get_mut(scheme_name)
        .expect(&err_msg)
        .0
        .as_mut()
        .downcast_mut::<T>()
        .expect(&err_msg2)
}

async fn shutdown_signal() {
    // Wait for the CTRL+C signal
    tokio::signal::ctrl_c()
        .await
        .expect("failed to install CTRL+C signal handler");
}

async fn build_store_instance(
    toml: &TomlFile,
    toml_file_path: impl AsRef<Path>,
    module: impl AsRef<Path>,
) -> Result<Builder> {
    let mut builder = Builder::from_module(module)?;
    let mut linked_capabilities: HashSet<String> = HashSet::new();
    let mut capability_store: HashMap<String, BasicState> = HashMap::new();

    builder.link_wasi()?;
    for c in toml.capability.as_ref().unwrap() {
        let resource_type = if toml.specversion == "0.1" {
            c.name.as_str()
        } else if toml.specversion == "0.2" {
            if let Some(r) = &c.resource {
                r.as_str()
            } else {
                bail!("missing resource field");
            }
        } else {
            bail!("unsupported toml spec version");
        };

        if resource_type != "http" {
            maybe_add_named_capability_to_store(
                &toml.specversion,
                toml.secret_store.clone(),
                &mut capability_store,
                c.clone(),
                &toml_file_path,
            )?;
        }

        match resource_type {
            #[cfg(feature = "keyvalue")]
            _ if KEYVALUE_HOST_IMPLEMENTORS.contains(&resource_type) => {
                if !linked_capabilities.contains("keyvalue") {
                    builder.link_capability::<Keyvalue>()?;
                    linked_capabilities.insert("keyvalue".to_string());
                }

                let resource = slight_keyvalue::Keyvalue::new(
                    resource_type.to_string(),
                    capability_store.clone(),
                );
                builder.add_to_builder("keyvalue".to_string(), resource);
            }
            #[cfg(feature = "distributed-locking")]
            _ if DISTRIBUTED_LOCKING_HOST_IMPLEMENTORS.contains(&resource_type) => {
                if !linked_capabilities.contains("distributed_locking") {
                    builder.link_capability::<DistributedLocking>()?;
                    linked_capabilities.insert("distributed_locking".to_string());
                }

                let resource = slight_distributed_locking::DistributedLocking::new(
                    resource_type.to_string(),
                    capability_store.clone(),
                );
                builder.add_to_builder("distributed_locking".to_string(), resource);
            }
            #[cfg(feature = "messaging")]
            _ if MESSAGING_HOST_IMPLEMENTORS.contains(&resource_type) => {
                if !linked_capabilities.contains("messaging") {
                    builder.link_capability::<Messaging>()?;
                    linked_capabilities.insert("messaging".to_string());
                }

                let resource =
                    slight_messaging::Messaging::new(&c.name, capability_store.clone()).await?;
                builder.add_to_builder("messaging".to_string(), resource);
            }
            #[cfg(feature = "runtime-configs")]
            _ if CONFIGS_HOST_IMPLEMENTORS.contains(&resource_type) => {
                if !linked_capabilities.contains("configs") {
                    builder.link_capability::<Configs>()?;
                    linked_capabilities.insert("configs".to_string());
                }

                let resource = slight_runtime_configs::Configs::new(
                    resource_type.to_string(),
                    capability_store.clone(),
                );
                builder.add_to_builder("configs".to_string(), resource);
            }
            #[cfg(feature = "sql")]
            _ if SQL_HOST_IMPLEMENTORS.contains(&resource_type) => {
                if !linked_capabilities.contains("sql") {
                    builder.link_capability::<Sql>()?;
                    linked_capabilities.insert("sql".to_string());
                }

                let resource =
                    slight_sql::Sql::new(resource_type.to_string(), capability_store.clone());
                builder.add_to_builder("sql".to_string(), resource);
            }
            #[cfg(feature = "http-server")]
            "http" => {
                if !linked_capabilities.contains("http") {
                    let http = slight_http_server::HttpServer::<Builder>::default();
                    builder
                        .link_capability::<HttpServer<Builder>>()?
                        .add_to_builder("http".to_string(), http);
                    linked_capabilities.insert("http".to_string());
                } else {
                    bail!("the http capability was already linked");
                }
            }
            #[cfg(feature = "http-client")]
            "http-client" => {
                if !linked_capabilities.contains("http-client") {
                    let http_client = HttpClient::new();
                    builder
                        .link_capability::<HttpClient>()?
                        .add_to_builder("http-client".to_string(), http_client);
                    linked_capabilities.insert("http-client".to_string());
                } else {
                    bail!("the http-client capability was already linked");
                }
            }
            _ => {
                bail!("invalid url: currently slight only supports 'configs.usersecrets', 'configs.envvars', 'keyvalue.filesystem', 'keyvalue.azblob', 'keyvalue.awsdynamodb', 'distributed_locking.etcd', 'messaging.confluent_apache_kafka', 'messaging.mosquitto', and 'http' schemes")
            }
        }
    }

    Ok(builder)
}

fn maybe_add_named_capability_to_store(
    specversion: &str,
    secret_store: Option<String>,
    capability_store: &mut HashMap<String, BasicState>,
    c: TomlCapability,
    toml_file_path: impl AsRef<Path>,
) -> Result<()> {
    if specversion == "0.1" {
        if let std::collections::hash_map::Entry::Vacant(e) = capability_store.entry(c.name.clone())
        {
            e.insert(BasicState::new(
                secret_store,
                c.name.clone(),
                c.name.clone(),
                c.configs,
                toml_file_path,
            ));
        } else {
            bail!("cannot add capabilities of the same name");
        }
    } else if specversion == "0.2" {
        if let std::collections::hash_map::Entry::Vacant(e) = capability_store.entry(c.name.clone())
        {
            let resource = if let Some(r) = c.resource {
                r
            } else {
                bail!("missing resource field");
            };

            e.insert(BasicState::new(
                None,
                resource,
                c.name.clone(),
                c.configs,
                toml_file_path,
            ));
        } else {
            bail!("cannot add capabilities of the same name");
        }
    } else {
        bail!("unsupported toml version");
    }

    Ok(())
}