
use actix_web::{ get, middleware, Error, web, App, HttpRequest, HttpResponse, HttpServer, };
use crate::AkriveiaState;
use crate::data_processor::OutUserData;
use futures::{ future::ok, Future, };
use std::sync::*;
use std::thread::*;

pub fn realtime_users(state: web::Data<Mutex<AkriveiaState>>, req: HttpRequest) -> impl Future<Item=HttpResponse, Error=Error> {
    let s = state.lock().unwrap();
    s.data_processor
        .send(OutUserData{})
        .then(|res| {
            match res {
                Ok(Ok(data)) => {
                    ok(HttpResponse::Ok().json(data))
                },
                _ => {
                    ok(HttpResponse::BadRequest().finish())
                }
        }})
}
