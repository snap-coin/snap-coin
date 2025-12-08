/// Core blockchain implementation, contains block validation and state storage logic
pub mod blockchain;

/// Core transaction struct, defines a transaction, and all its parameters
pub mod transaction;

/// Unspent transaction outputs, contains logic for verifying transactions in the context of utxos and applying (and reverting) changes to a utxo set.
pub mod utxo;

/// General utilities
pub mod utils;

/// A bunch of constants and helper functions that define economic parameters
pub mod economics;

/// Core block struct, defines a block and all of its parameters
pub mod block;

/// A struct for keeping, and updating network and transaction difficulty (POW)
pub mod difficulty;