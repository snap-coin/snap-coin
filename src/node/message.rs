use std::array::TryFromSliceError;

use bincode::{Decode, Encode};
use rand::random;
use thiserror::Error;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::tcp::{OwnedReadHalf, OwnedWriteHalf},
};

use crate::{
    core::{block::Block, transaction::Transaction}, crypto::Hash, version::VERSION
};

/// Struct that contains every command (request, response) sent on the p2p network
#[derive(Encode, Decode, Debug, Clone)]
pub enum Command {
    // Connect / keep-alive
    Connect,
    AcknowledgeConnection,
    Ping { height: usize },
    Pong { height: usize },
    GetPeers,
    SendPeers { peers: Vec<String> },

    // Live
    NewBlock { block: Block },
    NewTransaction { transaction: Transaction },

    // Historical
    GetBlock { block_hash: Hash },
    GetBlockResponse { block: Option<Block> },
    GetBlockHashes { start: usize, end: usize },
    GetBlockHashesResponse { block_hashes: Vec<Hash> },
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
    pub id: u16,
    pub command: Command,
}

impl Message {
    /// New 
    pub fn new(command: Command) -> Self {
        Message {
            version: VERSION,
            id: random(),
            command,
        }
    }

    pub fn make_response(&self, command: Command) -> Self {
        Message {
            version: VERSION,
            id: self.id,
            command,
        }
    }

    /// Serialize message into a Vec<u8> to be sent
    /// Message is serialized into: [8 bytes header (version 2, id 2, payload size 4)][payload]
    pub fn serialize(&self) -> Result<Vec<u8>, MessageError> {
        // Serialize just the command to get its size
        let command_bytes = bincode::encode_to_vec(&self.command, bincode::config::standard())?;
        let size: u32 = command_bytes.len() as u32;

        // Serialize the header first
        let mut header_bytes: Vec<u8> = Vec::new();
        header_bytes.extend_from_slice(&self.version.to_be_bytes());
        header_bytes.extend_from_slice(&self.id.to_be_bytes());
        header_bytes.extend_from_slice(&size.to_be_bytes());

        // Combine: [header][command]
        let mut message_bytes = Vec::new();
        message_bytes.extend_from_slice(&header_bytes);
        message_bytes.extend_from_slice(&command_bytes);

        Ok(message_bytes)
    }

    /// Send this message to a TcpStream (its owned write half)
    pub async fn send(&self, stream: &mut OwnedWriteHalf) -> Result<(), MessageError> {
        let buf = self.serialize()?;
        if let Err(e) = stream.write_all(&buf).await {
            return Err(e.into());
        }
        Ok(())
    }

    /// Read a message from a TcpStream (its owned read half)
    pub async fn from_stream(stream: &mut OwnedReadHalf) -> Result<Self, MessageError> {
        let mut header_bytes = [0u8; 8];
        if stream.read_exact(&mut header_bytes).await? != 8 {
            return Err(MessageError::HeaderLength);
        }

        let (version_bytes, id_and_size) = header_bytes.split_at(2);
        let (id_bytes, size_bytes) = id_and_size.split_at(2);

        let version = u16::from_be_bytes(version_bytes.try_into()?);
        let id = u16::from_be_bytes(id_bytes.try_into()?);
        let size = u32::from_be_bytes(size_bytes.try_into()?);

        let mut command_bytes = vec![0u8; size as usize];
        stream.read_exact(&mut command_bytes).await?;

        let command = bincode::decode_from_slice(&command_bytes, bincode::config::standard())?.0;
        Ok(Message {
            command,
            id,
            version,
        })
    }
}
