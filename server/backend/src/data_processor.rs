
use actix::prelude::*;
use actix_web::Result;
use std::io;
use std::collections::{ HashMap, BTreeMap, VecDeque };
use na;
use crate::db_utils;
use crate::models::beacon;
use crate::models::user;
use common::MacAddress;
use futures::future::{ ok, };

const LOCATION_HISTORY_SIZE: usize = 5;

// contains a vector of tag data from multiple beacons
#[derive(Debug)]
struct TagHistory {
    pub tag_name: String,
    pub tag_mac: MacAddress,
    pub beacon_history: BTreeMap<MacAddress, VecDeque<i64>>,
}

pub struct DataProcessor {
    // this hash maps the id_tag mac address to data points for that id tag.
    tag_hash: HashMap<MacAddress, Box<TagHistory>>,
    // TODO support floors
    // TODO init with db data?
    // this tree maps tag mac addresses to users
    // scanning the entire tree for all entries will likely be a very common,
    // so hash is likely not a good choice.
    users: BTreeMap<MacAddress, common::TrackedUser>
}

impl DataProcessor {
    pub fn new() -> DataProcessor {
        DataProcessor {
            tag_hash: HashMap::new(),
            users: BTreeMap::new(),
        }
    }

    fn calc_trilaterate(beacons: &Vec<common::Beacon>, tag_data: &Vec<common::TagData>, beacon_sources: &mut Vec<common::UserBeaconSourceLocations>) -> na::Vector2<f64> {
        if tag_data.len() < 3 {
            panic!("not enough data points to trilaterate");
        }
        if beacons.len() < 3 {
            panic!("not enough beacons to trilaterate");
        }
        assert!(beacon_sources.len() == 0);

        let env_factor = 2.0;
        let measure_power = -76.0;
        // TODO move to db
        let bloc1 = beacons[0].coordinates;
        let bloc2 = beacons[1].coordinates;
        let bloc3 = beacons[2].coordinates;

        // TODO change calc based on type
        let tag_distance0 = match tag_data[0].tag_distance {
            common::DataType::RSSI(rssi) => rssi,
            common::DataType::TOF(tof) => tof,
        } as f64;
        let tag_distance1 = match tag_data[1].tag_distance {
            common::DataType::RSSI(rssi) => rssi,
            common::DataType::TOF(tof) => tof,
        } as f64;
        let tag_distance2 = match tag_data[2].tag_distance {
            common::DataType::RSSI(rssi) => rssi,
            common::DataType::TOF(tof) => tof,
        } as f64;


        let denom = 10.0 * env_factor;
        let d1 = 10f64.powf((measure_power - tag_distance0) / denom);
        let d2 = 10f64.powf((measure_power - tag_distance1) / denom);
        let d3 = 10f64.powf((measure_power - tag_distance2) / denom);

        { // NOTE temporary
            beacon_sources.push(common::UserBeaconSourceLocations {
                name: beacons[0].name.clone(),
                location: bloc1,
                distance_to_tag: d1,
            });

            beacon_sources.push(common::UserBeaconSourceLocations {
                name: beacons[1].name.clone(),
                location: bloc2,
                distance_to_tag: d2,
            });

            beacon_sources.push(common::UserBeaconSourceLocations {
                name: beacons[2].name.clone(),
                location: bloc3,
                distance_to_tag: d3,
            });
        }

        // Trilateration solver
        let a = -2.0 * bloc1.x + 2.0 * bloc2.x;
        let b = -2.0 * bloc1.y + 2.0 * bloc2.y;
        let c = d1 * d1 - d2 * d2 - bloc1.x * bloc1.x + bloc2.x * bloc2.x - bloc1.y * bloc1.y + bloc2.y * bloc2.y;
        let d = -2.0 * bloc2.x + 2.0 * bloc3.x;
        let e = -2.0 * bloc2.y + 2.0 * bloc3.y;
        let f = d2 * d2 - d3 * d3 - bloc2.x * bloc2.x + bloc3.x * bloc3.x - bloc2.y * bloc2.y + bloc3.y * bloc3.y;

        let x = (c * e - f * b) / (e * a - b * d);
        let y = (c * d - a * f) / (b * d - a * e);

        na::Vector2::new(x as f64, y as f64)
    }
}

impl Actor for DataProcessor {
    type Context = Context<Self>;
}

pub enum DPMessage {
    ResetData, // Reset the stored data
}
impl Message for DPMessage {
    type Result = Result<u64, io::Error>;
}

impl Handler<DPMessage> for DataProcessor {
    type Result = Result<u64, io::Error>;

    fn handle (&mut self, msg: DPMessage, _: &mut Context<Self>) -> Self::Result {
        match msg {
            DPMessage::ResetData => {
                self.tag_hash.clear();
                self.users.clear();
            },
        }

        Ok(1)
    }
}

pub struct InLocationData(pub common::TagData);

impl Message for InLocationData {
    type Result = Result<(), ()>;
}
fn append_history(tag_entry: &mut Box<TagHistory>, rssi_value: i64, tag_data: &common::TagData) {
    if let Some(beacon_entry) = tag_entry.beacon_history.get_mut(&tag_data.beacon_mac) {
        beacon_entry.push_back(rssi_value);
        if beacon_entry.len() > LOCATION_HISTORY_SIZE {
            beacon_entry.pop_front();
        }
    } else {
        let mut deque = VecDeque::new();
        deque.push_back(rssi_value);
        tag_entry.beacon_history.insert(tag_data.beacon_mac.clone(), deque);
    }
}

impl Handler<InLocationData> for DataProcessor {
    type Result = ResponseActFuture<Self, (), ()>;

    fn handle (&mut self, msg: InLocationData, _: &mut Context<Self>) -> Self::Result {
        let tag_data = msg.0;
        let rssi_value = match tag_data.tag_distance {
            common::DataType::RSSI(rssi) => rssi,
            common::DataType::TOF(tof) => tof,
        };

        let opt_averages = match self.tag_hash.get_mut(&tag_data.tag_mac) {
            Some(mut tag_entry) => {
                append_history(&mut tag_entry, rssi_value, &tag_data);

                // TODO pick the most recent 3 beacons
                if tag_entry.beacon_history.len() >= 3 {
                    let averaged_data: Vec<common::TagData> = tag_entry.beacon_history.iter().map(|(beacon_mac, hist_vec)| {
                        common::TagData {
                            tag_mac: tag_entry.tag_mac.clone(),
                            tag_name: tag_entry.tag_name.clone(),
                            beacon_mac: beacon_mac.clone(),
                            // TODO recasting back to RSSI is silly... refactor...
                            tag_distance: common::DataType::RSSI(hist_vec.into_iter().sum::<i64>() / hist_vec.len() as i64),
                        }
                    }).collect();

                    Some(averaged_data)
                } else {
                    None
                }
            },
            None => {
                // create new entry
                let mut hash_entry = TagHistory {
                    tag_name: tag_data.tag_name.clone(),
                    tag_mac: tag_data.tag_mac.clone(),
                    beacon_history: BTreeMap::new(),
                };
                let mut deque = VecDeque::new();
                deque.push_back(rssi_value);
                hash_entry.beacon_history.insert(tag_data.beacon_mac.clone(), deque);
                self.tag_hash.insert(tag_data.tag_mac.clone(), Box::new(hash_entry));
                None
            },
        };

        let fut = match opt_averages {
            Some(averages) => {
                let beacon_macs: Vec<MacAddress>  = averages.iter().map(|tagdata| tagdata.beacon_mac).collect();
                actix::fut::Either::A(db_utils::default_connect()
                    .and_then(|client| {
                        beacon::select_beacons_by_mac(client, beacon_macs)
                    })
                    .into_actor(self)
                    .and_then(move |(client, beacons), actor, _context| {
                        let mut beacon_sources: Vec<common::UserBeaconSourceLocations> = Vec::new();
                        let new_tag_location = Self::calc_trilaterate(&beacons, &averages, &mut beacon_sources);
                        // update the user information
                        match actor.users.get_mut(&averages[0].tag_mac) {
                            Some(user_ref) => {
                                user_ref.beacon_sources = beacon_sources;
                                user_ref.coordinates = new_tag_location;
                            },
                            None => {
                                // TODO this should probably eventually be an error if the user
                                // is missing, but for now just make the user instead
                                let mut user = common::TrackedUser::new();
                                user.mac_address = averages[0].tag_mac.clone();
                                user.beacon_sources = beacon_sources;
                                user.coordinates = new_tag_location;
                                actor.users.insert(averages[0].tag_mac.clone(), user);
                            }
                        }
                        user::update_user_coords_by_mac(client, averages[0].tag_mac, new_tag_location)
                            .map(|(_client, _opt_user)| {
                            })
                            .into_actor(actor)
                    })
                )
            },
            None => {
                actix::fut::Either::B(ok(()).into_actor(self))
            }
        };

        Box::new(fut.map_err(|_postgres_err, _, _| { }))
    }
}

pub struct OutUserData { }

impl Message for OutUserData {
    type Result = Result<Vec<common::TrackedUser>, io::Error>;
}

impl Handler<OutUserData> for DataProcessor {
    type Result = Result<Vec<common::TrackedUser>, io::Error>;

    fn handle (&mut self, _msg: OutUserData, _: &mut Context<Self>) -> Self::Result {
        Ok(self.users.values().cloned().collect())
    }
}
