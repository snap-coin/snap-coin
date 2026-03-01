use std::array::TryFromSliceError;

use bincode::{Decode, Encode};
use rand::random;
use thiserror::Error;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::tcp::{OwnedReadHalf, OwnedWriteHalf},
};

use crate::{
    core::{
        block::{Block, BlockMetadata},
        transaction::{Transaction, TransactionId},
    },
    crypto::{Hash, merkle_tree::MerkleTreeProof},
    version::VERSION,
};

pub const METADATA_FETCH_MAX_COUNT: usize = 1024;
pub const CURRENT_NETWORK_ID: u32 = 20072009;

/// Struct that contains every command (request, response) sent on the p2p network
#[derive(Encode, Decode, Debug, Clone)]
pub enum Command {
    // Connect / keep-alive
    Connect,
    AcknowledgeConnection,
    Ping {
        height: usize,
    },
    Pong {
        height: usize,
    },
    GetPeers,
    SendPeers {
        peers: Vec<String>,
    },

    // Live
    NewBlock {
        block: Block,
    },
    NewBlockResolved,
    NewTransaction {
        transaction: Transaction,
    },
    NewTransactionResolved,

    // Historical
    GetBlockMetadata {
        block_hash: Hash,
    },
    GetBlockMetadataResponse {
        block_metadata: Option<BlockMetadata>,
    },
    GetBlockMetadatas {
        range_start: usize,
        range_end: usize,
    },
    GetBlockMetadatasResponse {
        block_metadatas: Vec<BlockMetadata>,
    },
    GetBlock {
        block_hash: Hash,
    },
    GetBlockResponse {
        block: Option<Block>,
    },
    GetBlockHashes {
        start: usize,
        end: usize,
    },
    GetBlockHashesResponse {
        block_hashes: Vec<Hash>,
    },

    // Light node
    GetTransactionMerkleProof {
        block: Hash,
        transaction_id: TransactionId,
    },
    GetTransactionMerkleProofResponse {
        proof: Option<MerkleTreeProof>,
    },
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

    #[error("Incorrect network")]
    IncorrectNetwork,
}

pub type MessageId = u32;

#[derive(Debug, Clone)]
pub struct Message {
    pub version: u16,
    pub id: MessageId,
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
    /// Message is serialized into: `[14 bytes header (network_id: 4, version 2, id 4, payload size 4)][payload]`
    pub fn serialize(&self) -> Result<Vec<u8>, MessageError> {
        // Serialize just the command to get its size
        let command_bytes = bincode::encode_to_vec(&self.command, bincode::config::standard())?;
        let size: u32 = command_bytes.len() as u32;

        // Serialize the header first
        let mut header_bytes: Vec<u8> = Vec::new();
        header_bytes.extend_from_slice(&self.version.to_be_bytes());
        header_bytes.extend_from_slice(CURRENT_NETWORK_ID.to_be_bytes());
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
        // info!("TX: {:#?}", self.command);
        Ok(())
    }

    /// Read a message from a TcpStream (its owned read half)
    // Header layout:
    // [version:2][network_id:4][id:4][size:4] = 14 bytes
    pub async fn from_stream(stream: &mut OwnedReadHalf) -> Result<Self, MessageError> {
        let mut header_bytes = [0u8; 14];
        if stream.read_exact(&mut header_bytes).await? != 14 {
            return Err(MessageError::HeaderLength);
        }

        let (version_bytes, rest) = header_bytes.split_at(2);
        let (network_bytes, rest) = rest.split_at(4);
        let (id_bytes, size_bytes) = rest.split_at(4);

        let version = u16::from_be_bytes(version_bytes.try_into()?);
        let network_id = u32::from_be_bytes(network_bytes.try_into()?);
        let id = MessageId::from_be_bytes(id_bytes.try_into()?);
        let size = u32::from_be_bytes(size_bytes.try_into()?);

        // Network validation
        if network_id != CURRENT_NETWORK_ID {
            return Err(MessageError::IncorrectNetwork);
        }

        let mut command_bytes = vec![0u8; size as usize];
        stream.read_exact(&mut command_bytes).await?;

        let command = bincode::decode_from_slice(&command_bytes, bincode::config::standard())?.0;

        Ok(Message {
            version,
            id,
            command,
        })
    }
}
