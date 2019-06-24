#![feature(proc_macro_hygiene, decl_macro)]

#[macro_use] extern crate rocket;
#[macro_use] extern crate serde_derive;

use std::error::Error;
use std::time::SystemTime;

use chrono::prelude::*;

use google_geocoding::WGS84;

use rocket::{State, Outcome};
use rocket::http::Status;
use rocket::request::{self, FromRequest, Request};
use rocket_contrib::json::Json;

use mongodb::{bson, doc, Client, ThreadedClient, ClientOptions};
use mongodb::coll::options::FindOptions;
use mongodb::db::ThreadedDatabase;

#[derive(Debug)]
struct User {
    id: u64,
}

#[derive(Debug)]
enum UserError {
    Missing,
    NotFound,
    Malformed(Box<dyn std::error::Error>),
    Invalid,
}

fn validate(key: u64) -> bool {
    key == 111
}

impl<'a, 'r> FromRequest<'a, 'r> for User {
    type Error = UserError;

    fn from_request(request: &'a Request<'r>) -> request::Outcome<Self, Self::Error> {
        let keys: Vec<_> = request.headers().get("x-api-key").collect();
        match keys.len() {
            0 => Outcome::Failure((Status::BadRequest, UserError::Missing)),
            1 => match keys[0].parse().map(|id| (validate(id), id)) {
                    Ok((true, id)) => Outcome::Success(User { id }),
                    Ok((false, _)) => Outcome::Failure((Status::BadRequest, UserError::Invalid)),
                    Err(err) =>
                        Outcome::Failure((Status::BadRequest, UserError::Malformed(Box::new(err)))),
            },
            _ => Outcome::Failure((Status::BadRequest, UserError::Invalid)),
        }
    }
}

const DATABASE: &'static str = "gps";
const COLLECTION: &'static str = "locations";

#[derive(Serialize, Deserialize, Debug)]
struct Location {
    lng: f32,
    lat: f32,
}

#[derive(Serialize, Deserialize, Debug)]
struct TimestampLocation {
    timestamp: String,
    location: Location,
}

#[derive(Serialize, Deserialize, Debug)]
struct Kilometers(f32);

#[derive(Serialize, Deserialize, Debug)]
enum QueryResponse {
    Missing,
    Location(TimestampLocation),
}

#[derive(Serialize, Deserialize, Debug)]
enum UpdateResponse {
    Initial,
    DistTraveled(Kilometers),
}


#[put("/loc", format="json", data="<location>")]
fn update_location(db_client: State<Client>, user: User, location: Json<Location>)
    -> Json<UpdateResponse>
{
    // retrieve previous location
    let last_loc = last_location(&*db_client, &user);

    // insert new location
    let coll = db_client.db(DATABASE).collection(COLLECTION);
    let update = doc! {
        "uid": user.id,
        "timestamp": DateTime::<Utc>::from(SystemTime::now()),
        "lng": location.lng,
        "lat": location.lat,
    };
    coll.insert_one(update, None).expect("insert failed");
    println!("Location update for {:?}: {:?}\n", user, location);

    match last_loc {
        Some(prev_loc) => {
            // compute distance traveled
            let prev_loc = WGS84::new(prev_loc.location.lat, prev_loc.location.lng, 0.0);
            let new_loc = WGS84::new(location.lat, location.lng, 0.0);
            Json(UpdateResponse::DistTraveled(Kilometers(prev_loc.distance(&new_loc) / 1000.0)))
        },
        None => Json(UpdateResponse::Initial),
    }
}

fn last_loc_opts() -> FindOptions {
    let mut opts = FindOptions::new();
    opts.sort = Some(doc!{ "timestamp": -1 });
    opts.limit = Some(1);
    opts
}

fn last_location(db_client: &Client, user: &User) -> Option<TimestampLocation> {
    let coll = db_client.db(DATABASE).collection(COLLECTION);
    let mut cursor = coll.find(
        Some(doc!{ "uid":user.id  }),
        Some(last_loc_opts()),
    ).expect("find failed");

    cursor.next().map(|cursor_result| {
        let item = cursor_result.expect("cursor failure");
        let timestamp = item.get_utc_datetime("timestamp").expect("timestamp missing")
            .to_rfc3339();
        TimestampLocation {
            timestamp,
            location: Location {
                lng: item.get_f64("lng").expect("lng missing") as f32,
                lat: item.get_f64("lat").expect("lat missing") as f32,
            }
        }
    })
}

#[get("/loc")]
fn query_location(db_client: State<Client>, user: User) -> Json<QueryResponse> {
    match last_location(&*db_client, &user) {
        Some(ts_loc) => {
            println!("Last location for {}: {:?}", user.id, ts_loc);
            Json(QueryResponse::Location(ts_loc))
        },
        None => {
            println!("Location missing for {}", user.id);
            Json(QueryResponse::Missing)
        }
    }
}

#[derive(Debug)]
pub enum ArgError {
    Url,
    User,
    Pass,
}
impl std::fmt::Display for ArgError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}", self.description())
    }
}
impl Error for ArgError {
    fn description(&self) -> &str {
        match *self {
            ArgError::Url => "missing argument: URL",
            ArgError::User => "missing argument: User",
            ArgError::Pass => "missing argument: Password",
        }
    }
}


fn main() -> Result<(), Box<dyn Error>>{
    let mut args_iter = std::env::args().into_iter();
    args_iter.next(); // skip binary name
    let url = args_iter.next().ok_or(ArgError::Url)?;
    let user = args_iter.next().ok_or(ArgError::User)?;
    let pass = args_iter.next().ok_or(ArgError::Pass)?;

    let client = Client::with_uri_and_options(
        &url,
        ClientOptions::with_unauthenticated_ssl(None, false)
    )?;

    // authenticate
    let db = client.db("admin");
    db.auth(&user, &pass)?;

    rocket::ignite()
        .mount("/", routes![update_location, query_location])
        .manage(client)
        .launch();

    Ok(())
}
