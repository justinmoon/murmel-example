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
use std::{net::SocketAddr, path::Path, str::FromStr, time::SystemTime};

pub mod blockdownload;
pub mod echo;
pub mod spv;

use spv::Constructor;

pub fn run(
    network: Network,
    connections: usize,
    birth: u64, //db_file: Option<PathBuf>,
    peers: Vec<SocketAddr>,
) {
    let listeners: Vec<SocketAddr> = vec![];
    let path = match network {
        Network::Bitcoin => Some(Path::new("data/mainnet/client.db")),
        Network::Testnet => Some(Path::new("data/testnet/client.db")),
        Network::Regtest => Some(Path::new("data/regtest/client.db")),
    };
    let chain_db = Constructor::open_db(path, network, birth).unwrap();
    let mut spv = Constructor::new(network, listeners, chain_db).unwrap();
    spv.run(network, peers, connections)
        .expect("can not start node");
}

fn testnet() {
    simple_logger::init_with_level(Level::Debug).unwrap();
    let network = Network::Testnet;
    let connections = 4;
    let peers: Vec<SocketAddr> = vec![];
    let birth = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    run(network, connections, birth, peers);
}

fn regtest() {
    simple_logger::init_with_level(Level::Info).unwrap();
    let network = Network::Regtest;
    let connections = 4;
    let peers: Vec<SocketAddr> = vec![SocketAddr::from_str("127.0.0.1:18444").unwrap()];
    let birth = 0;
    run(network, connections, birth, peers);
}

fn main() {
    regtest();
}
