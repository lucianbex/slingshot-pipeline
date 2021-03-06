use merlin::Transcript;
use serde::{Deserialize, Serialize};
use std::fmt;
use zkvm::encoding::*;
use zkvm::{merkle, Hash, MerkleItem, MerkleTree, Tx, VerifiedTx};

use super::state::BlockchainState;
use super::utreexo::{self, Proof};
use readerwriter::Encodable;

/// Identifier of the block, computed as a hash of the `BlockHeader`.
#[derive(Clone, Copy, PartialEq, Default)]
pub struct BlockID(pub [u8; 32]);
serialize_bytes32!(BlockID);

/// Witness hash of the transaction that commits to all signatures and proofs.
#[derive(Clone, Copy, PartialEq, Default)]
pub struct WitnessHash(pub [u8; 32]);
serialize_bytes32!(WitnessHash);

/// BlockHeader contains the metadata for the block of transactions,
/// committing to them, but not containing the actual transactions.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct BlockHeader {
    /// Network version.
    pub version: u64,
    /// Serial number of the block, starting with 1.
    pub height: u64,
    /// ID of the previous block. Initial block uses the all-zero string.
    pub prev: BlockID,
    /// Integer timestamp of the block in milliseconds since the Unix epoch:
    /// 00:00:00 UTC Jan 1, 1970.
    pub timestamp_ms: u64,
    /// 32-byte Merkle root of the transaction witness hashes (`BlockTx::witness_hash`) in the block.
    pub txroot: Hash,
    /// 32-byte Merkle root of the Utreexo state.
    pub utxoroot: Hash,
    /// Extra data for the future extensions.
    pub ext: Vec<u8>,
}

/// Transaction annotated with Utreexo proofs.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BlockTx {
    /// ZkVM transaction.
    pub tx: Tx,
    /// Utreexo proofs.
    pub proofs: Vec<utreexo::Proof>,
}

/// Block that was successfully processed and applied to the state,
/// and can be used by the rest of the system to index txs, update wallet data etc.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VerifiedBlock {
    /// Block header
    pub header: BlockHeader,
    /// Utreexo state
    pub utreexo: utreexo::Forest,
    /// Utreexo catchup map for updating the proofs
    pub catchup: utreexo::Catchup,
    /// List of verified transactions
    pub raw_txs: Vec<BlockTx>,
    /// List of verified transactions
    pub verified_txs: Vec<VerifiedTx>,
}

impl BlockHeader {
    /// Computes the ID of the block header.
    pub fn id(&self) -> BlockID {
        let mut t = Transcript::new(b"ZkVM.blockheader");
        t.append_u64(b"version", self.version);
        t.append_u64(b"height", self.height);
        t.append_message(b"previd", &self.prev.0);
        t.append_u64(b"timestamp_ms", self.timestamp_ms);
        t.append_message(b"txroot", &self.txroot.0);
        t.append_message(b"utxoroot", &self.utxoroot.0);
        t.append_message(b"ext", &self.ext);

        let mut result = [0u8; 32];
        t.challenge_bytes(b"id", &mut result);
        BlockID(result)
    }

    /// Creates an initial block header.
    pub fn make_initial(timestamp_ms: u64, utxoroot: Hash) -> BlockHeader {
        BlockHeader {
            version: 1,
            height: 1,
            prev: BlockID([0; 32]),
            timestamp_ms,
            txroot: MerkleTree::empty_root(b"ZkVM.txroot"),
            utxoroot,
            ext: Vec::new(),
        }
    }
}

impl BlockTx {
    /// Hash of the witness data (tx program, r1cs proof, signature, utreexo proofs)
    pub fn witness_hash(&self) -> WitnessHash {
        let mut t = Transcript::new(b"ZkVM.tx_witness_hash");
        t.append_message(b"tx", &self.encode_to_vec());
        let mut result = [0u8; 32];
        t.challenge_bytes(b"hash", &mut result);
        WitnessHash(result)
    }
}

impl Encodable for BlockHeader {
    fn encode(&self, w: &mut impl Writer) -> Result<(), WriteError> {
        w.write_u64(b"version", self.version)?;
        w.write_u64(b"height", self.height)?;
        w.write(b"prev", &self.prev)?;
        w.write_u64(b"timestamp_ms", self.timestamp_ms)?;
        w.write(b"txroot", &self.txroot)?;
        w.write(b"utxoroot", &self.utxoroot)?;
        w.write_u32(b"ext_len", self.ext.len() as u32)?;
        w.write(b"ext", &self.ext)?;
        Ok(())
    }
}

impl ExactSizeEncodable for BlockHeader {
    fn encoded_size(&self) -> usize {
        8 + 8 + 32 + 8 + 32 + 32 + 4 + self.ext.len()
    }
}

impl Decodable for BlockHeader {
    fn decode(buf: &mut impl Reader) -> Result<Self, ReadError> {
        Ok(BlockHeader {
            version: buf.read_u64()?,
            height: buf.read_u64()?,
            prev: buf.read_u8x32().map(BlockID)?,
            timestamp_ms: buf.read_u64()?,
            txroot: buf.read_u8x32().map(Hash)?,
            utxoroot: buf.read_u8x32().map(Hash)?,
            ext: {
                let n = buf.read_u32()? as usize;
                buf.read_bytes(n)?
            },
        })
    }
}

impl Encodable for BlockTx {
    fn encode(&self, w: &mut impl Writer) -> Result<(), WriteError> {
        self.tx.encode(w)?;
        w.write_size(b"n", self.proofs.len())?;
        for proof in self.proofs.iter() {
            match proof {
                Proof::Transient => w.write_u8(b"type", 0)?,
                Proof::Committed(path) => {
                    w.write_u8(b"type", 1)?;
                    path.encode(w)?;
                }
            }
        }
        Ok(())
    }
}

impl ExactSizeEncodable for BlockTx {
    /// Returns the size in bytes required to serialize the `Tx`.
    fn encoded_size(&self) -> usize {
        self.tx.encoded_size()
            + 4
            + self
                .proofs
                .iter()
                .map(|proof| match proof {
                    Proof::Transient => 1,
                    Proof::Committed(path) => 1 + path.encoded_size(),
                })
                .sum::<usize>()
    }
}

impl Decodable for BlockTx {
    fn decode(r: &mut impl Reader) -> Result<Self, ReadError> {
        let tx = Tx::decode(r)?;
        let n = r.read_size()? as usize;
        let proofs = r.read_vec(n, |r| match r.read_u8()? {
            0 => Ok(Proof::Transient),
            1 => merkle::Path::decode(r)
                .map(Proof::Committed)
                .map_err(|_| ReadError::InvalidFormat),
            _ => Err(ReadError::InvalidFormat),
        })?;
        Ok(BlockTx { tx, proofs })
    }
}

impl MerkleItem for WitnessHash {
    fn commit(&self, t: &mut Transcript) {
        t.append_message(b"txwit", &self.0);
    }
}

impl AsRef<[u8]> for WitnessHash {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl core::ops::Deref for WitnessHash {
    type Target = [u8];
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<[u8]> for BlockID {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl core::ops::Deref for BlockID {
    type Target = [u8];
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl fmt::Debug for BlockID {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "BlockID({})", hex::encode(&self.0))
        // Without hex crate we'd do this, but it outputs comma-separated numbers: [aa, 11, 5a, ...]
        // write!(f, "{:x?}", &self.0)
    }
}

impl fmt::Debug for WitnessHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "WitnessHash({})", hex::encode(&self.0))
        // Without hex crate we'd do this, but it outputs comma-separated numbers: [aa, 11, 5a, ...]
        // write!(f, "{:x?}", &self.0)
    }
}

impl VerifiedBlock {
    /// Returns the blockchain state produced by this block
    pub fn blockchain_state(&self) -> BlockchainState {
        BlockchainState {
            tip: self.header.clone(),
            utreexo: self.utreexo.clone(),
        }
    }
}
