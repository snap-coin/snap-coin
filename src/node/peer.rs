use std::{sync::Arc, time::Duration};
use thiserror::Error;
use tokio::{net::{TcpStream, tcp::{OwnedReadHalf, OwnedWriteHalf}}, sync::RwLock, task::JoinHandle, time::sleep};
use anyhow::Result;

use crate::node::{message::{Command, Message, MessageError}, node::Node};

#[derive(Error, Debug)]
pub enum PeerError {
  #[error("Tcp connection to node was not successful")]
  NodeTcpConnection(#[from] std::io::Error),

  #[error("{0}")]
  MessageError(#[from] MessageError),
}

#[derive(Clone, Debug)]
pub struct Stream {
  pub reader: Arc<RwLock<OwnedReadHalf>>,
  pub writer: Arc<RwLock<OwnedWriteHalf>>
}

impl From<TcpStream> for Stream {
  fn from(stream: TcpStream) -> Stream {
    let (reader, writer) = stream.into_split();
    Stream { reader: Arc::new(RwLock::new(reader)), writer: Arc::new(RwLock::new(writer)) }
  }
}

#[derive(Clone, Debug)]
pub struct Peer {
  pub address: String,
  pub stream: Option<Arc<Stream>>
}

impl Peer {
  pub async fn connect<F, FutF, H, FutH>(
    &mut self,
    on_peer_fail: Arc<F>,
    handle_message: Arc<H>,
    node: Arc<RwLock<Node>>,
    stream: TcpStream,
    is_client: bool
  ) -> Result<JoinHandle<()>, PeerError>
  where
    F: Fn(Arc<RwLock<Node>>, String) -> FutF + Send + Sync + 'static,
    FutF: Future<Output = ()> + Send + 'static,
    H: Fn(Message, Arc<RwLock<Node>>, Arc<Stream>) -> FutH + Send + Sync + 'static,
    FutH: Future<Output = Option<Message>> + Send + 'static,
  {
    // Connect to peer
    let stream: Arc<Stream> = Arc::new(stream.into());

    self.stream = Some(Arc::clone(&stream));

    // Send initial connect message
    if !is_client {
      Message::new(Command::Connect).send(&mut *stream.writer.write().await).await?;
      Message::from_stream(&mut *stream.reader.write().await).await?; // Wait for response
    }

    let stream_receive = Arc::clone(&stream);
    let node_receive = Arc::clone(&node);
    let on_peer_fail_receive = Arc::clone(&on_peer_fail);
    let address = self.address.clone();
    let message_handle = tokio::spawn(async move { // Receiving loop
      if let Err(e) = async {
        loop {
          let message = Message::from_stream(&mut *stream_receive.reader.write().await).await?; // receive message from peer
          // let manager handle message and produce response
          let response: Option<Message> = handle_message(message, Arc::clone(&node_receive), Arc::clone(&stream_receive)).await;
          match response {
            Some(res_msg) => {
              res_msg.send(&mut *stream_receive.writer.write().await).await?;
            },
            None => {},
          }
        }

        #[allow(unreachable_code)]
        Ok::<(), anyhow::Error>(())
      }.await {
        eprintln!("Peer listener failed: {}", e);
        on_peer_fail_receive(node_receive, address).await;
      }
    });

    let address = self.address.clone();
    let stream_ping = Arc::clone(&stream);
    let node_ping = Arc::clone(&node);
    let on_peer_fail_ping = Arc::clone(&on_peer_fail);
    let ping_handle = tokio::spawn(async move {
      if let Err(e) = async {
        loop {
          sleep(Duration::from_secs(5)).await;

          let height = node_ping.read().await.blockchain.get_height();
          let message = Message::new(Command::Ping { height });
          message.send(&mut *stream_ping.writer.write().await).await?;
        }

        #[allow(unreachable_code)]
        Ok::<(), anyhow::Error>(())
      }.await {
        eprintln!("Peer pinger failed: {}", e);
        on_peer_fail_ping(node_ping, address).await;
      }
    });
    let task = tokio::spawn(async move {
      let (r1, r2) = tokio::join!(message_handle, ping_handle);

      if let Err(e) = r1 {
        eprintln!("message_handle failed: {}", e);
      }
      if let Err(e) = r2 {
        eprintln!("ping_handle failed: {}", e);
      }
    });
    Ok(task)
  }
}
