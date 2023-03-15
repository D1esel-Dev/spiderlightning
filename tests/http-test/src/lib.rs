use anyhow::Result;

use http_server::*;
use slight_http_handler_macro::register_handler;
use slight_http_server_macro::on_server_init;

wit_bindgen_rust::import!("../../wit/http-server.wit");
wit_bindgen_rust::export!("../../wit/http-server-export.wit");
wit_bindgen_rust::import!("../../wit/http-client.wit");
wit_error_rs::impl_error!(http_server::HttpRouterError);
wit_error_rs::impl_error!(http_client::HttpError);

#[on_server_init]
fn main() -> Result<()> {
    let router = Router::new()?;
    let router_with_route = router
        .get("/hello", "handle_hello")?
        .get("/person/:name", "handle_person_with_name")?
        .get("/foo", "handle_foo")?
        .put("/bar", "handle_bar")?
        .post("/upload", "upload")?
        .delete("/delete-file", "delete_file_handler")?
        .get("/request", "handle_request")?;

    println!("guest starting server");
    let _ = Server::serve("0.0.0.0:3000", &router_with_route)?;
    // server.stop().unwrap();
    println!("guest moving on");

    Ok(())
}

#[register_handler]
fn handle_hello(req: Request) -> Result<Response, HttpError> {
    Ok(Response {
        headers: Some(req.headers),
        body: Some("hello".as_bytes().to_vec()),
        status: 200,
    })
}

#[register_handler]
fn handle_person_with_name(req: Request) -> Result<Response, HttpError> {
    let id = req.params.into_iter().find(|x| x.0 == "name").unwrap_or(("".into(), "".into()));
    Ok(Response {
        headers: Some(req.headers),
        body: Some(format!("hello: {}", id.1).as_bytes().to_vec()),
        status: 200,
    })
}

#[register_handler]
fn handle_foo(request: Request) -> Result<Response, HttpError> {
    Ok(Response {
        headers: Some(request.headers),
        body: request.body,
        status: 500,
    })
}

#[register_handler]
fn handle_bar(request: Request) -> Result<Response, HttpError> {
    assert_eq!(request.method, Method::Put);
    Ok(Response {
        headers: Some(request.headers),
        body: request.body,
        status: 200,
    })
}

#[register_handler]
fn delete_file_handler(request: Request) -> Result<Response, HttpError> {
    assert_eq!(request.method, Method::Delete);
    Ok(Response {
        headers: Some(request.headers),
        body: request.body,
        status: 200,
    })
}


#[register_handler]
fn upload(request: Request) -> Result<Response, HttpError> {
    assert_eq!(request.method, Method::Post);
    Ok(Response {
        headers: Some(request.headers),
        body: request.body,
        status: 200,
    })
}

#[register_handler]
fn handle_request(_request: Request) -> Result<Response, HttpError> {
    let req = crate::http_client::Request {
        method: crate::http_client::Method::Get,
        uri: "https://some-random-api.ml/facts/cat",
        headers: &[],
        body: None,
        params: &[],
    };
    let res = crate::http_client::request(req).unwrap();
    let res = Response {
        status: res.status,
        headers: res.headers,
        body: res.body,
    };
    Ok(res)
}