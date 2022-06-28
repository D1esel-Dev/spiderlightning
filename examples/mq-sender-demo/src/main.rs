use anyhow::Result;

use mq::*;
wit_bindgen_rust::import!("../../wit/mq.wit");
wit_error_rs::impl_error!(Error);

fn main() -> Result<()> {
    let resource_descriptor = get_mq("wasi-cloud-queue")?;
    for _ in 0..3 {
        println!("sending \"hello, world!\" to queue");
        send(&resource_descriptor, "hello, world!".as_bytes())?;
    }

    Ok(())
}