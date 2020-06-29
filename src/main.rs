//
// Copyright 2018-2019 Tamas Blummer
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
//
extern crate bitcoin;
extern crate log;
extern crate murmel;
extern crate rand;
extern crate simple_logger;

use bitcoin::network::constants::Network;
use log::Level;
use std::{net::SocketAddr, path::Path, time::SystemTime};

pub mod echo;
pub mod spv;

use spv::Constructor;

pub fn run(
    network: Network,
    connections: usize,
    birth: u64, //db_file: Option<PathBuf>,
) {
    let peers: Vec<SocketAddr> = vec![];
    let listeners: Vec<SocketAddr> = vec![];
    let chain_db = Constructor::open_db(Some(&Path::new("client.db")), network, birth).unwrap();
    let mut spv = Constructor::new(network, listeners, chain_db).unwrap();
    spv.run(network, peers, connections)
        .expect("can not start node");
}

fn main() {
    simple_logger::init_with_level(Level::Debug).unwrap();
    let network = Network::Testnet;
    let connections = 4;
    let birth = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    run(network, connections, birth);
}
