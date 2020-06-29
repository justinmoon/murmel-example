use bitcoin::network::message::NetworkMessage;
use log::info;
use std::{sync::mpsc, thread, time::Duration};

use murmel::p2p::{P2PControlSender, PeerMessage, PeerMessageReceiver, PeerMessageSender};

pub struct Echo {}

impl Echo {
    pub fn new(p2p: P2PControlSender<NetworkMessage>) -> PeerMessageSender<NetworkMessage> {
        let (sender, receiver) = mpsc::sync_channel(p2p.back_pressure);
        let mut ping = Echo {};

        thread::Builder::new()
            .name("echo".to_string())
            .spawn(move || ping.run(receiver))
            .unwrap();

        PeerMessageSender::new(sender)
    }

    fn run(&mut self, receiver: PeerMessageReceiver<NetworkMessage>) {
        loop {
            while let Ok(msg) = receiver.recv_timeout(Duration::from_millis(1000)) {
                let result = match msg {
                    PeerMessage::Incoming(pid, msg) => {
                        let cmd = match msg {
                            NetworkMessage::Version(_) => "version",
                            NetworkMessage::Verack => "verack",
                            NetworkMessage::Addr(_) => "addr",
                            NetworkMessage::Inv(_) => "inv",
                            NetworkMessage::GetData(_) => "getdata",
                            NetworkMessage::NotFound(_) => "notfound",
                            NetworkMessage::GetBlocks(_) => "getblocks",
                            NetworkMessage::GetHeaders(_) => "getheaders",
                            NetworkMessage::MemPool => "mempool",
                            NetworkMessage::Tx(_) => "tx",
                            NetworkMessage::Block(_) => "block",
                            NetworkMessage::Headers(_) => "headers",
                            NetworkMessage::SendHeaders => "sendheaders",
                            NetworkMessage::GetAddr => "getaddr",
                            NetworkMessage::Ping(_) => "ping",
                            NetworkMessage::Pong(_) => "pong",
                            NetworkMessage::GetCFilters(_) => "getcfilters",
                            NetworkMessage::CFilter(_) => "cfilter",
                            NetworkMessage::GetCFHeaders(_) => "getcfheaders",
                            NetworkMessage::CFHeaders(_) => "cfheaders",
                            NetworkMessage::GetCFCheckpt(_) => "getcfckpt",
                            NetworkMessage::CFCheckpt(_) => "cfcheckpt",
                            NetworkMessage::Alert(_) => "alert",
                            NetworkMessage::Reject(_) => "reject",
                        };
                        Some((pid, cmd))
                    }
                    _ => None,
                };
                if let Some((pid, cmd)) = result {
                    info!("{} sent {}", pid, cmd);
                }
            }
        }
    }
}
