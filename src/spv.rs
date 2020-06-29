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

use bitcoin::blockdata::transaction::TxOut;
use bitcoin::network::constants::Network;
use bitcoin::network::message::{NetworkMessage, RawNetworkMessage};
use bitcoin::{Address, BitcoinHash, Block, BlockHeader, OutPoint, Script};
use bitcoin_hashes::{hex::FromHex, sha256d};
use futures::{
    executor::{ThreadPool, ThreadPoolBuilder},
    future,
    task::{Context, SpawnExt},
    Future, FutureExt, Poll as Async, StreamExt,
};
use futures_timer::Interval;
use log::info;
use rand::{thread_rng, RngCore};
use std::pin::Pin;
use std::time::Duration;
use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    path::Path,
    sync::{atomic::AtomicUsize, mpsc, Arc, Mutex, RwLock},
};

use murmel::chaindb::{ChainDB, SharedChainDB};
use murmel::dispatcher::Dispatcher;
use murmel::dns::dns_seed;
use murmel::downstream::{DownStreamDummy, Downstream, SharedDownstream};
use murmel::error::Error;
use murmel::headerdownload::HeaderDownload;
use murmel::p2p::{BitcoinP2PConfig, P2PControl, PeerMessageSender, PeerSource, P2P};
use murmel::ping::Ping;
use murmel::timeout::Timeout;

use std::{collections::VecDeque, thread};

use bitcoin::network::message_blockdata::{GetHeadersMessage, InvType, Inventory};
use log::{debug, trace};
use murmel::p2p::{P2PControlSender, PeerId, PeerMessage, PeerMessageReceiver, SERVICE_BLOCKS};
use murmel::timeout::{ExpectedReply, SharedTimeout};

const MAX_PROTOCOL_VERSION: u32 = 70001;

pub struct Tracker {
    script_pubkey: Script,
    utxos: HashMap<OutPoint, TxOut>,
}

impl Downstream for Tracker {
    fn block_connected(&mut self, block: &Block, _height: u32) {
        let initial_balance = self.balance();
        for tx in block.txdata.iter() {
            for (index, vout) in tx.output.iter().enumerate() {
                let is_mine = self.script_pubkey == vout.script_pubkey;
                let outpoint = OutPoint::new(tx.txid(), index as u32);
                if is_mine {
                    info!("receive: {}", outpoint);
                    self.utxos.insert(outpoint, vout.clone());
                }
            }
            for input in tx.input.iter() {
                if self.utxos.contains_key(&input.previous_output) {
                    info!("spend: {}", &input.previous_output);
                    self.utxos.remove(&input.previous_output).unwrap();
                }
            }
        }
        let final_balance = self.balance();
        if initial_balance != final_balance {
            info!("balance change: {} -> {}", initial_balance, final_balance);
        }
    }

    fn header_connected(&mut self, header: &BlockHeader, _height: u32) {
        println!("\n\n\nHEADER: {:?}\n\n\n", header.bitcoin_hash());
    }

    fn block_disconnected(&mut self, header: &BlockHeader) {
        // TODO: revert block_connected
        println!("\n\n\nDISCONNECT: {:?}\n\n\n", header.bitcoin_hash());
    }
}

impl Tracker {
    pub fn new() -> Self {
        let script_pubkey = Script::from(
            Vec::<u8>::from_hex("76a91402306a7c23f3e8010de41e9e591348bb83f11daa88ac").unwrap(),
        );
        let address = Address::from_script(&script_pubkey, Network::Regtest).unwrap();
        println!("address: {}", address);
        let utxos = HashMap::new();
        Self {
            script_pubkey,
            utxos,
        }
    }

    fn balance(&self) -> u64 {
        self.utxos.values().fold(0u64, |sum, val| sum + val.value)
    }
}

#[derive(Clone)]
struct KeepConnected {
    cex: ThreadPool,
    dns: Vec<SocketAddr>,
    earlier: HashSet<SocketAddr>,
    p2p: Arc<P2P<NetworkMessage, RawNetworkMessage, BitcoinP2PConfig>>,
    min_connections: usize,
}

impl Future for KeepConnected {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Async<Self::Output> {
        if self.p2p.n_connected_peers() < self.min_connections {
            let eligible = self
                .dns
                .iter()
                .cloned()
                .filter(|a| !self.earlier.contains(a))
                .collect::<Vec<_>>();
            if eligible.len() > 0 {
                let mut rng = thread_rng();
                let choice = eligible[(rng.next_u32() as usize) % eligible.len()];
                self.earlier.insert(choice.clone());
                let add = self
                    .p2p
                    .add_peer("bitcoin", PeerSource::Outgoing(choice))
                    .map(|_| ());
                self.cex
                    .spawn(add)
                    .expect("can not add peer for outgoing connection");
            }
        }
        Async::Ready(())
    }
}

pub struct BlockDownload {
    p2p: P2PControlSender<NetworkMessage>,
    chaindb: SharedChainDB,
    timeout: SharedTimeout<NetworkMessage, ExpectedReply>,
    downstream: SharedDownstream,
    blocks_wanted: VecDeque<(sha256d::Hash, u32)>,
    blocks_asked: VecDeque<(sha256d::Hash, u32)>,
    block_download_peer: Option<PeerId>,
    birth: u64,
}

impl BlockDownload {
    pub fn new(
        chaindb: SharedChainDB,
        p2p: P2PControlSender<NetworkMessage>,
        timeout: SharedTimeout<NetworkMessage, ExpectedReply>,
        downstream: SharedDownstream,
        processed_block: Option<sha256d::Hash>,
        birth: u64,
    ) -> PeerMessageSender<NetworkMessage> {
        let (sender, receiver) = mpsc::sync_channel(p2p.back_pressure);

        let mut blocks_wanted = VecDeque::new();
        {
            let chaindb = chaindb.read().unwrap();
            if let Some(mut h) = chaindb.header_tip() {
                if (h.stored.header.time as u64) > birth {
                    let stop_at = processed_block.unwrap_or_default();
                    let mut block_hash = h.bitcoin_hash();
                    while block_hash != stop_at {
                        blocks_wanted.push_front((block_hash, h.stored.height));
                        block_hash = h.stored.header.prev_blockhash.clone();
                        if block_hash != sha256d::Hash::default() {
                            h = chaindb
                                .get_header(&block_hash)
                                .expect("inconsistent header cache");
                            if (h.stored.header.time as u64) < birth {
                                break;
                            }
                        }
                    }
                }
            }
        }

        let mut headerdownload = BlockDownload {
            chaindb,
            p2p,
            timeout,
            downstream,
            blocks_wanted,
            blocks_asked: VecDeque::new(),
            block_download_peer: None,
            birth,
        };

        thread::Builder::new()
            .name("header download".to_string())
            .spawn(move || headerdownload.run(receiver))
            .unwrap();

        PeerMessageSender::new(sender)
    }

    fn run(&mut self, receiver: PeerMessageReceiver<NetworkMessage>) {
        loop {
            while let Ok(msg) = receiver.recv_timeout(Duration::from_millis(1000)) {
                match msg {
                    PeerMessage::Connected(pid, _) => {
                        if self.is_serving_blocks(pid) {
                            trace!("serving blocks peer={}", pid);
                            self.get_headers(pid);
                            if self.block_download_peer.is_none() {
                                debug!("new block download peer={}", pid);
                                self.block_download_peer = Some(pid);
                            }
                        }
                    }
                    PeerMessage::Disconnected(pid, _) => {
                        if self.block_download_peer.is_some() {
                            if pid == self.block_download_peer.unwrap() {
                                self.block_download_peer = None;
                                debug!("lost block download peer={}", pid);
                                while let Some(asked) = self.blocks_asked.pop_back() {
                                    self.blocks_wanted.push_front(asked);
                                }
                            }
                        }
                    }
                    PeerMessage::Incoming(pid, msg) => {
                        match msg {
                            NetworkMessage::Headers(ref headers) => {
                                if self.is_serving_blocks(pid) {
                                    self.headers(headers, pid);
                                }
                            }
                            NetworkMessage::Inv(ref inv) => {
                                if self.is_serving_blocks(pid) {
                                    self.inv(inv, pid);
                                }
                            }
                            NetworkMessage::Block(ref block) => self.block(block, pid),
                            _ => {}
                        }
                        if self.block_download_peer.is_none() {
                            self.block_download_peer = Some(pid);
                        }
                        if pid == self.block_download_peer.unwrap() {
                            self.ask_blocks(pid)
                        }
                    }
                    _ => {}
                }
            }
            self.timeout
                .lock()
                .unwrap()
                .check(vec![ExpectedReply::Headers, ExpectedReply::Block]);
        }
    }

    fn ask_blocks(&mut self, pid: PeerId) {
        let mut timeout = self.timeout.lock().unwrap();
        if !timeout.is_busy_with(pid, ExpectedReply::Block) {
            let mut n_entries = 0;
            while let Some((hash, height)) = self.blocks_wanted.pop_front() {
                self.blocks_asked.push_back((hash, height));
                n_entries += 1;
                if n_entries == 1000 {
                    break;
                }
            }
            if self.blocks_asked.len() > 0 {
                self.p2p.send_network(
                    pid,
                    NetworkMessage::GetData(
                        self.blocks_asked
                            .iter()
                            .map(|(hash, _)| Inventory {
                                inv_type: InvType::Block,
                                hash: hash.clone(),
                            })
                            .collect(),
                    ),
                );
                debug!("asked {} blocks from peer={}", self.blocks_asked.len(), pid);
                timeout.expect(pid, self.blocks_asked.len(), ExpectedReply::Block);
            }
        } else {
            debug!("still waiting for blocks from peer={}", pid);
        }
    }

    fn block(&mut self, block: &Block, pid: PeerId) {
        if let Some(download_peer) = self.block_download_peer {
            if download_peer == pid {
                if let Some((expected, height)) = self.blocks_asked.front() {
                    let height = *height;
                    if block.header.bitcoin_hash() == *expected {
                        // will drop for out of sequence answers
                        self.timeout
                            .lock()
                            .unwrap()
                            .received(pid, 1, ExpectedReply::Block);

                        self.blocks_asked.pop_front();
                        let mut downstream = self.downstream.lock().unwrap();
                        downstream.block_connected(block, height);
                    }
                }
            }
        }
    }

    fn is_serving_blocks(&self, peer: PeerId) -> bool {
        if let Some(peer_version) = self.p2p.peer_version(peer) {
            return peer_version.services & SERVICE_BLOCKS != 0;
        }
        false
    }

    // process an incoming inventory announcement
    fn inv(&mut self, v: &Vec<Inventory>, peer: PeerId) {
        let mut ask_for_headers = false;
        for inventory in v {
            // only care for blocks
            if inventory.inv_type == InvType::Block {
                let chaindb = self.chaindb.read().unwrap();
                if chaindb.get_header(&inventory.hash).is_none() {
                    debug!(
                        "received inv for new block {} peer={}",
                        inventory.hash, peer
                    );
                    // ask for header(s) if observing a new block
                    ask_for_headers = true;
                }
            }
        }
        if ask_for_headers {
            self.get_headers(peer);
        }
    }

    /// get headers this peer is ahead of us
    fn get_headers(&mut self, peer: PeerId) {
        if self
            .timeout
            .lock()
            .unwrap()
            .is_busy_with(peer, ExpectedReply::Headers)
        {
            return;
        }
        let chaindb = self.chaindb.read().unwrap();
        let locator = chaindb.header_locators();
        if locator.len() > 0 {
            let first = if locator.len() > 0 {
                *locator.first().unwrap()
            } else {
                sha256d::Hash::default()
            };
            self.timeout
                .lock()
                .unwrap()
                .expect(peer, 1, ExpectedReply::Headers);
            self.p2p.send_network(
                peer,
                NetworkMessage::GetHeaders(GetHeadersMessage::new(locator, first)),
            );
        }
    }

    fn headers(&mut self, headers: &Vec<BlockHeader>, peer: PeerId) {
        self.timeout
            .lock()
            .unwrap()
            .received(peer, 1, ExpectedReply::Headers);

        if headers.len() > 0 {
            // current height
            let mut height;
            // some received headers were not yet known
            let mut some_new = false;
            let mut moved_tip = None;
            {
                let chaindb = self.chaindb.read().unwrap();

                if let Some(tip) = chaindb.header_tip() {
                    height = tip.stored.height;
                } else {
                    return;
                }
            }

            let mut headers_queue = VecDeque::new();
            headers_queue.extend(headers.iter());
            while !headers_queue.is_empty() {
                let mut connected_headers = Vec::new();
                let mut disconnected_headers = Vec::new();
                {
                    let mut chaindb = self.chaindb.write().unwrap();
                    while let Some(header) = headers_queue.pop_front() {
                        // add to blockchain - this also checks proof of work
                        match chaindb.add_header(&header) {
                            Ok(Some((stored, unwinds, forwards))) => {
                                connected_headers.push((stored.height, stored.header));
                                // POW is ok, stored top chaindb
                                some_new = true;

                                if let Some(forwards) = forwards {
                                    moved_tip = Some(forwards.last().unwrap().clone());
                                }
                                height = stored.height;

                                if let Some(unwinds) = unwinds {
                                    disconnected_headers.extend(
                                        unwinds
                                            .iter()
                                            .map(|h| chaindb.get_header(h).unwrap().stored.header),
                                    );
                                    break;
                                }
                            }
                            Ok(None) => {}
                            Err(Error::SpvBadProofOfWork) => {
                                info!("Incorrect POW, banning peer={}", peer);
                                self.p2p.ban(peer, 100);
                            }
                            Err(e) => {
                                debug!("error {} processing header {} ", e, header.bitcoin_hash());
                            }
                        }
                    }
                    chaindb.batch().unwrap();
                }

                // call downstream outside of chaindb lock
                let mut downstream = self.downstream.lock().unwrap();
                for header in &disconnected_headers {
                    if (header.time as u64) > self.birth {
                        self.blocks_wanted.pop_back();
                        downstream.block_disconnected(header);
                    }
                }
                for (height, header) in &connected_headers {
                    if (header.time as u64) > self.birth {
                        self.blocks_wanted
                            .push_back((header.bitcoin_hash(), *height));
                        downstream.header_connected(header, *height);
                    }
                }
            }

            if some_new {
                // ask if peer knows even more
                self.get_headers(peer);
            }

            if let Some(new_tip) = moved_tip {
                info!(
                    "received {} headers new tip={} from peer={}",
                    headers.len(),
                    new_tip,
                    peer
                );
                self.p2p.send(P2PControl::Height(height));
            } else {
                debug!(
                    "received {} known or orphan headers [{} .. {}] from peer={}",
                    headers.len(),
                    headers[0].bitcoin_hash(),
                    headers[headers.len() - 1].bitcoin_hash(),
                    peer
                );
            }
        }
    }
}

/// The complete stack
pub struct Spv {
    p2p: Arc<P2P<NetworkMessage, RawNetworkMessage, BitcoinP2PConfig>>,
    /// this should be accessed by Lightning
    pub downstream: SharedDownstream,
}

impl Spv {
    /// open DBs
    pub fn open_db(
        path: Option<&Path>,
        network: Network,
        _birth: u64,
    ) -> Result<SharedChainDB, Error> {
        let mut chaindb = if let Some(path) = path {
            ChainDB::new(path, network)?
        } else {
            ChainDB::mem(network)?
        };
        chaindb.init()?;
        Ok(Arc::new(RwLock::new(chaindb)))
    }

    /// Construct the stack
    pub fn new(
        network: Network,
        listen: Vec<SocketAddr>,
        chaindb: SharedChainDB,
    ) -> Result<Spv, Error> {
        const BACK_PRESSURE: usize = 10;

        let (to_dispatcher, from_p2p) = mpsc::sync_channel(BACK_PRESSURE);

        let p2pconfig = BitcoinP2PConfig {
            network,
            nonce: thread_rng().next_u64(),
            max_protocol_version: MAX_PROTOCOL_VERSION,
            user_agent: "murmel: 0.1.0".to_owned(),
            height: AtomicUsize::new(0),
            server: !listen.is_empty(),
        };

        let (p2p, p2p_control) = P2P::new(
            p2pconfig,
            PeerMessageSender::new(to_dispatcher),
            BACK_PRESSURE,
        );

        let shared_dummy = Arc::new(Mutex::new(DownStreamDummy {}));

        let timeout = Arc::new(Mutex::new(Timeout::new(p2p_control.clone())));

        let mut dispatcher = Dispatcher::new(from_p2p);

        dispatcher.add_listener(HeaderDownload::new(
            chaindb.clone(),
            p2p_control.clone(),
            timeout.clone(),
            shared_dummy.clone(),
        ));
        dispatcher.add_listener(Ping::new(p2p_control.clone(), timeout.clone()));

        // FIXME
        let processed_block: Option<sha256d::Hash> = None;
        let birth = 0;

        let shared_tracker = Arc::new(Mutex::new(Tracker::new()));

        dispatcher.add_listener(BlockDownload::new(
            chaindb.clone(),
            p2p_control.clone(),
            timeout.clone(),
            shared_tracker.clone(),
            processed_block,
            birth,
        ));

        for addr in &listen {
            p2p_control.send(P2PControl::Bind(addr.clone()));
        }

        Ok(Spv {
            p2p,
            downstream: shared_dummy.clone(),
        })
    }

    /// Run the stack. This should be called AFTER registering listener of the ChainWatchInterface,
    /// so they are called as the stack catches up with the blockchain
    /// * peers - connect to these peers at startup (might be empty)
    /// * min_connections - keep connections with at least this number of peers. Peers will be randomly chosen
    /// from those discovered in earlier runs
    pub fn run(
        &mut self,
        network: Network,
        peers: Vec<SocketAddr>,
        min_connections: usize,
    ) -> Result<(), Error> {
        let mut executor = ThreadPoolBuilder::new()
            .name_prefix("bitcoin-connect")
            .pool_size(2)
            .create()
            .expect("can not start futures thread pool");

        let p2p = self.p2p.clone();
        for addr in &peers {
            executor
                .spawn(
                    p2p.add_peer("bitcoin", PeerSource::Outgoing(addr.clone()))
                        .map(|_| ()),
                )
                .expect("can not spawn task for peers");
        }

        let keep_connected = KeepConnected {
            min_connections,
            p2p: self.p2p.clone(),
            earlier: HashSet::new(),
            dns: dns_seed(network),
            cex: executor.clone(),
        };
        executor
            .spawn(Interval::new(Duration::new(10, 0)).for_each(move |_| keep_connected.clone()))
            .expect("can not keep connected");

        let p2p = self.p2p.clone();
        let mut cex = executor.clone();
        executor.run(future::poll_fn(move |_| {
            let needed_services = 0;
            p2p.poll_events("bitcoin", needed_services, &mut cex);
            Async::Ready(())
        }));
        Ok(())
    }
}
