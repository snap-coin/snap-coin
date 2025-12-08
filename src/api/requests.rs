use std::net::SocketAddr;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::{io::AsyncReadExt, net::TcpStream};

use crate::{
    core::{
        block::Block,
        blockchain::BlockchainError,
        transaction::{Transaction, TransactionId, TransactionOutput},
    },
    crypto::{Hash, keys::Public},
};

#[derive(Error, Debug, Serialize, Deserialize)]
pub enum RequestResponseError {
    #[error("Failed to deserialize message")]
    DecodingFailed,

    #[error("Failed to serialize message")]
    EncodingFailed,

    #[error("Malformed data error")]
    MalformedData,

    #[error("Stream error")]
    Stream,

    #[error("Request returned invalid response")]
    IncorrectResponse,
}

#[derive(Serialize, Deserialize)]
pub enum Request {
    Height,
    Block { block_hash: Hash },
    Difficulty,
    BlockHash { height: u64 },
    BlockHeight { hash: Hash },
    Transaction { transaction_id: TransactionId },
    TransactionsOfAddress { address: Public },
    AvailableUTXOs { address: Public },
    Balance { address: Public },
    Reward,
    Peers,
    Mempool,
    NewBlock { new_block: Block },
    NewTransaction { new_transaction: Transaction },
}

impl Request {
    /// Serialize into a u8 buffer (first 2 bytes = length)
    pub fn encode(self) -> Result<Vec<u8>, RequestResponseError> {
        let request_string =
            serde_json::to_string(&self).map_err(|_| RequestResponseError::EncodingFailed)?;
        let request_bytes = request_string.as_bytes();
        let request_length = request_bytes.len();

        if request_length > u16::MAX as usize {
            return Err(RequestResponseError::MalformedData);
        }

        let mut buf = vec![0u8; request_length + 2];
        buf[0..2].copy_from_slice(&(request_length as u16).to_be_bytes());
        buf[2..].copy_from_slice(request_bytes);

        Ok(buf)
    }

    /// Blocking deserialize from TcpStream
    pub async fn decode_from_stream(
        stream: &mut TcpStream,
    ) -> Result<Self, RequestResponseError> {
        let mut size_buf = [0u8; 2];
        stream
            .read_exact(&mut size_buf)
            .await
            .map_err(|_| RequestResponseError::Stream)?;

        let request_size = u16::from_be_bytes(size_buf) as usize;
        let mut request_buf = vec![0u8; request_size];

        stream
            .read_exact(&mut request_buf)
            .await
            .map_err(|_| RequestResponseError::Stream)?;

        let request: Request = serde_json::from_slice(&request_buf)
            .map_err(|_| RequestResponseError::DecodingFailed)?;

        Ok(request)
    }
}

#[derive(Serialize, Deserialize)]
pub enum Response {
    Height {
        height: u64,
    },
    Block {
        block: Option<Block>,
    },
    Difficulty {
        transaction_difficulty: [u8; 32],
        block_difficulty: [u8; 32],
    },
    BlockHash {
        hash: Option<Hash>,
    },
    BlockHeight {
        height: Option<usize>,
    },
    Transaction {
        transaction: Option<Transaction>,
    },
    TransactionsOfAddress {
        transactions: Vec<TransactionId>,
    },
    AvailableUTXOs {
        available_inputs: Vec<(TransactionId, TransactionOutput, usize)>,
    },
    Balance {
        balance: u64,
    },
    Reward {
        reward: u64,
    },
    Peers {
        peers: Vec<SocketAddr>,
    },
    Mempool {
        mempool: Vec<Transaction>,
    },
    NewBlock {
        status: Result<(), BlockchainError>,
    },
    NewTransaction {
        status: Result<(), BlockchainError>,
    },
}

impl Response {
    /// Serialize into a u8 buffer (first 2 bytes = length)
    pub fn encode(self) -> Result<Vec<u8>, RequestResponseError> {
        let response_string =
            serde_json::to_string(&self).map_err(|_| RequestResponseError::EncodingFailed)?;
        let response_bytes = response_string.as_bytes();
        let response_length = response_bytes.len();

        if response_length > u16::MAX as usize {
            return Err(RequestResponseError::MalformedData);
        }

        let mut buf = vec![0u8; response_length + 2];
        buf[0..2].copy_from_slice(&(response_length as u16).to_be_bytes());
        buf[2..].copy_from_slice(response_bytes);

        Ok(buf)
    }

    /// Blocking deserialize from TcpStream
    pub async fn decode_from_stream(stream: &mut TcpStream) -> Result<Self, RequestResponseError> {
        let mut size_buf = [0u8; 2];
        stream
            .read_exact(&mut size_buf)
            .await
            .map_err(|_| RequestResponseError::Stream)?;

        let response_size = u16::from_be_bytes(size_buf) as usize;
        let mut response_buf = vec![0u8; response_size];

        stream
            .read_exact(&mut response_buf)
            .await
            .map_err(|_| RequestResponseError::Stream)?;

        let response: Response = serde_json::from_slice(&response_buf)
            .map_err(|_| RequestResponseError::DecodingFailed)?;

        Ok(response)
    }
}
