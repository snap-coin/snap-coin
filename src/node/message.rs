use std::array::TryFromSliceError;

use bincode::{Decode, Encode};
use thiserror::Error;
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::tcp::{OwnedReadHalf, OwnedWriteHalf}};

use crate::{core::{block::Block, crypto::Hash, transaction::Transaction}, version::VERSION};

#[derive(Encode, Decode, Debug, Clone)]
pub enum Command {
  // Connect / keep-alive
  Connect,
  AcknowlageConnection,
  Ping { height: usize },
  Pong { height: usize },
  GetPeers,
  SendPeers { peers: Vec<String> },
  
  // Live
  NewBlock { block: Block },
  NewTransaction { transaction: Transaction },
  
  // Historical
  GetBlock { block_hash: Hash },
  SendBlock { block: Option<Block> },
  GetBlockHashes { start: usize, end: usize },
  SendBlockHashes { block_hashes: Vec<Hash> }
}

#[derive(Error, Debug)]
pub enum MessageError {
  #[error("Failed to encode command")]
  Encoding(#[from] bincode::error::EncodeError),

  #[error("Failed to decode command")]
  Decoding(#[from] bincode::error::DecodeError),

  #[error("Failed to write or read to / from stream")]
  Stream(#[from] std::io::Error),

  #[error("Received header length is not correct")]
  HeaderLength,

  #[error("Received header version or size bytes length is not correct")]
  HeaderItemLength(#[from] TryFromSliceError),
}

#[derive(Debug, Clone)]
pub struct Message {
  pub version: u16,
  pub command: Command
}

// Message is serialized into: [6 bytes header (version, payload size)][payload]

impl Message {
  pub fn new(command: Command) -> Self {
    Message {
      version: VERSION,
      command
    }
  }

  pub fn serialize(&self) -> Result<Vec<u8>, MessageError> {
    // Serialize just the command to get its size
    let command_bytes = bincode::encode_to_vec(&self.command, bincode::config::standard())?;
    let size: u32 = command_bytes.len() as u32;

    // Serialize the header first
    let mut header_bytes: Vec<u8> = Vec::new();
    header_bytes.extend_from_slice(&self.version.to_be_bytes());
    header_bytes.extend_from_slice(&size.to_be_bytes());

    // Combine: [header][command]
    let mut message_bytes = Vec::new();
    message_bytes.extend_from_slice(&header_bytes);
    message_bytes.extend_from_slice(&command_bytes);

    Ok(message_bytes)
  }

  pub async fn send(&self, stream: &mut OwnedWriteHalf) -> Result<(), MessageError> {
    let buf = self.serialize()?;
    if let Err(e) = stream.write_all(&buf).await {
      println!("Socket write failed: kind={:?}, {:?}", e.kind(), e);
      return Err(e.into());
    }
    Ok(())
  }

  pub async fn from_stream(stream: &mut OwnedReadHalf) -> Result<Self, MessageError> {
    let mut header_bytes = [0u8; 6];
    if stream.read_exact(&mut header_bytes).await? != 6 {
      return Err(MessageError::HeaderLength);
    }

    let (version_bytes, size_bytes) = header_bytes.split_at(2);

    let version = u16::from_be_bytes(version_bytes.try_into()?);
    let size = u32::from_be_bytes(size_bytes.try_into()?);

    let mut command_bytes = vec![0u8; size as usize];
    stream.read_exact(&mut command_bytes).await?;

    let command = bincode::decode_from_slice(&command_bytes, bincode::config::standard())?.0;
    println!("Got message {:?}", command);
    Ok(Message {
      command,
      version
    })
  }
}