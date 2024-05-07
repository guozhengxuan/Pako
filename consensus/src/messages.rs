use crate::config::{Committee, EpochNumber, ViewNumber};
use crate::error::{ConsensusError, ConsensusResult};
use crypto::{Digest, Hash, PublicKey, Signature, SignatureService};
use ed25519_dalek::Digest as _;
use ed25519_dalek::Sha512;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::convert::TryInto;
use std::fmt;
use threshold_crypto::{PublicKeySet, SignatureShare};

#[macro_export]
macro_rules! digest {
    ($($x: expr),+) => {
        {
            let mut hasher = Sha512::new();
            $(
                hasher.update($x);
            )+
            Digest(hasher.finalize().as_slice()[..32].try_into().unwrap())
        }
    };
}

// Two PB phase, Phase1 for block bradcasting, Phase2 for commit vector sharing.
#[derive(Serialize, Deserialize, Clone, Copy)]
pub enum PBPhase {
    Phase1,
    Phase2,
}

impl fmt::Display for PBPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Phase1 => write!(f, "PBPhase1"),
            Self::Phase2 => write!(f, "PBPhase2"),
        }
    }
}

pub type Sigma = Option<threshold_crypto::Signature>;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum ConsensusMessage {
    Val(Val),
    Echo(Echo),
    Finish(Finish),
    Halt(Halt),
    RandomnessShare(RandomnessShare),
    RandomCoin(RandomCoin),
    Done(Done),
    // RequestHelp(EpochNumber, PublicKey, PublicKey),
    // Help(Block),
}

impl fmt::Display for ConsensusMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ConsensusMessage {{ {} }}",
            match &self {
                ConsensusMessage::Val(_) => "VAL",
                ConsensusMessage::Echo(_) => "ECHO",
                ConsensusMessage::Finish(_) => "FINISH",
                ConsensusMessage::Halt(_) => "HALT",
                ConsensusMessage::RandomnessShare(_) => "RANDOMNESS_SHARE",
                ConsensusMessage::RandomCoin(_) => "RANDOM_COIN",
                ConsensusMessage::Done(_) => "PREVOTE",
                // ConsensusMessage::RequestHelp(_, _, _) => "REQUEST_HELP",
                // ConsensusMessage::Help(_) => "HELP",
            }
        )
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum Val {
    Block(Block),
    CommitVector(CommitVector),
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Block {
    pub payload: Vec<Digest>,
    pub author: PublicKey,
    pub signature: Signature,
    pub epoch: EpochNumber,
    pub proof: Sigma,
}

impl Block {
    pub async fn new(
        payload: Vec<Digest>,
        author: PublicKey,
        epoch: EpochNumber,
        proof: Sigma,
        mut signature_service: SignatureService,
    ) -> Self {
        let block = Self {
            payload,
            author,
            signature: Signature::default(),
            epoch,
            proof,
        };
        let signature = signature_service.request_signature(block.digest()).await;
        Self { signature, ..block }
    }

    pub fn verify(
        &self,
        committee: &Committee,
        halt_mark: EpochNumber,
        epochs_halted: &HashSet<EpochNumber>,
    ) -> ConsensusResult<()> {
        // Discard block with halted epoch number.
        ensure!(
            self.epoch > halt_mark && !epochs_halted.contains(&self.epoch),
            ConsensusError::MessageWithHaltedEpoch(self.epoch, halt_mark + 1)
        );

        // Ensure the authority has voting rights.
        let voting_rights = committee.stake(&self.author);
        ensure!(
            voting_rights > 0,
            ConsensusError::UnknownAuthority(self.author)
        );

        // Check signature.
        self.signature.verify(&self.digest(), &self.author)?;

        Ok(())
    }

    pub fn check_sigma(&self, pk: &threshold_crypto::PublicKey) -> bool {
        if let Some(sigma) = &self.proof {
            return pk.verify(&sigma, self.digest());
        }
        false
    }
}

impl Hash for Block {
    fn digest(&self) -> Digest {
        let mut hasher = Sha512::new();
        hasher.update(self.author.0);
        hasher.update(self.epoch.to_le_bytes());
        self.payload.iter().for_each(|p| hasher.update(p));
        hasher.update(match &self.proof {
            Some(_) => &[1],
            _ => &[0],
        });
        Digest(hasher.finalize().as_slice()[..32].try_into().unwrap())
    }
}

impl fmt::Debug for Block {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        write!(
            f,
            "{}: Block(author {}, epoch {}, has_qc {}, payload_len {})",
            self.digest(),
            self.author,
            self.epoch,
            match self.proof {
                Some(_) => "Yes",
                _ => "No",
            },
            self.payload.iter().map(|x| x.size()).sum::<usize>(),
        )
    }
}

impl fmt::Display for Block {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        write!(f, "B{}", self.author)
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct CommitVector {
    pub epoch: EpochNumber,
    pub author: PublicKey,
    pub signature: Signature,
    pub received: Vec<PublicKey>,
    pub proof: Sigma,
}

impl CommitVector {
    pub async fn new(
        epoch: EpochNumber,
        author: PublicKey,
        received: Vec<PublicKey>,
        proof: Sigma,
        mut signature_service: SignatureService,
    ) -> Self {
        let cv = CommitVector {
            epoch,
            author,
            signature: Signature::default(),
            received,
            proof,
        };
        let signature = signature_service.request_signature(cv.digest()).await;
        Self { signature, ..cv }
    }

    pub fn verify(
        &self,
        committee: &Committee,
        halt_mark: EpochNumber,
        epochs_halted: &HashSet<EpochNumber>,
    ) -> ConsensusResult<()> {
        // Discard block with halted epoch number.
        ensure!(
            self.epoch > halt_mark && !epochs_halted.contains(&self.epoch),
            ConsensusError::MessageWithHaltedEpoch(self.epoch, halt_mark + 1)
        );

        // Ensure the authority has voting rights.
        let voting_rights = committee.stake(&self.author);
        ensure!(
            voting_rights > 0,
            ConsensusError::UnknownAuthority(self.author)
        );

        // Check signature.
        self.signature.verify(&self.digest(), &self.author)?;

        // Check valid format.
        let mut seen = HashSet::new();
        let count = self.received.iter().fold(0, |acc, pk| {
            if committee.authorities.contains_key(pk) && !seen.contains(pk) {
                seen.insert(pk);
                acc + 1
            } else {
                acc
            }
        });
        ensure!(
            count >= committee.quorum_threshold(),
            ConsensusError::InvalidCommitVector(self.author)
        );
        Ok(())
    }

    pub fn check_sigma(&self, pk: &threshold_crypto::PublicKey) -> bool {
        if let Some(sigma) = &self.proof {
            return pk.verify(&sigma, self.digest());
        }
        false
    }
}

impl Hash for CommitVector {
    fn digest(&self) -> Digest {
        let mut hasher = Sha512::new();
        hasher.update(self.epoch.to_le_bytes());
        hasher.update(self.author.0);
        hasher.update(match &self.proof {
            Some(_) => &[1],
            _ => &[0],
        });
        let mut tuples = self.received.iter().collect::<Vec<_>>();
        tuples.sort_by_key(|e| e.0);
        tuples.into_iter().for_each(|pk| {
            hasher.update(pk.0);
        });

        Digest(hasher.finalize().as_slice()[..32].try_into().unwrap())
    }
}

impl fmt::Debug for CommitVector {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        write!(
            f,
            "CommitVector(author {}, epoch {}, has_qc {})",
            self.author,
            self.epoch,
            match self.proof {
                Some(_) => "Yes",
                _ => "No",
            },
        )
    }
}

impl fmt::Display for CommitVector {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        write!(f, "CV{}", self.author)
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Echo {
    // Block info.
    pub digest: Digest,
    pub digest_author: PublicKey,
    pub phase: PBPhase,

    // Echo info.
    pub epoch: EpochNumber,
    pub author: PublicKey,

    // Signature share against block digest.
    pub signature_share: SignatureShare,
}

impl Echo {
    pub async fn new(
        digest: Digest,
        digest_author: PublicKey,
        phase: PBPhase,
        epoch: EpochNumber,
        author: PublicKey,
        mut signature_service: SignatureService,
    ) -> Self {
        let signature_share = signature_service
            .request_tss_signature(digest.clone())
            .await
            .unwrap();
        Self {
            digest,
            digest_author,
            phase,
            epoch,
            author,
            signature_share,
        }
    }
    pub fn verify(
        &self,
        committee: &Committee,
        pk_set: &PublicKeySet,
        digest_author: PublicKey,
        halt_mark: EpochNumber,
        epochs_halted: &HashSet<EpochNumber>,
    ) -> ConsensusResult<()> {
        // Check for epoch.
        ensure!(
            self.epoch > halt_mark && !epochs_halted.contains(&self.epoch),
            ConsensusError::MessageWithHaltedEpoch(self.epoch, halt_mark + 1)
        );

        // Verify leader.
        ensure!(
            self.digest_author == digest_author,
            ConsensusError::WrongLeader {
                digest: self.digest.clone(),
                leader: self.digest_author,
                author: digest_author,
                epoch: self.epoch,
            }
        );

        // Ensure the authority has voting rights.
        ensure!(
            committee.stake(&self.author) > 0,
            ConsensusError::UnknownAuthority(self.author)
        );

        let pk_share = pk_set.public_key_share(committee.id(self.author));
        // Check the signature share.
        ensure!(
            pk_share.verify(&self.signature_share, &self.digest),
            ConsensusError::InvalidSignatureShare(self.author)
        );

        Ok(())
    }
}

impl fmt::Debug for Echo {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        write!(
            f,
            "Echo(author {}, digest_author {}, epoch {}, phase {}",
            self.author, self.digest_author, self.epoch, self.phase,
        )
    }
}

impl Hash for Echo {
    fn digest(&self) -> Digest {
        // Echo is distinguished by <epoch, phase>,
        digest!(
            self.epoch.to_le_bytes(),
            match self.phase {
                PBPhase::Phase1 => &[0],
                PBPhase::Phase2 => &[1],
            },
            "ECHO"
        )
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Finish(pub Val);

impl Hash for Finish {
    fn digest(&self) -> Digest {
        // Finish is distinguished by <epoch, view, FINISH>,
        let mut hasher = Sha512::new();
        match &self.0 {
            Val::Block(block) => {
                hasher.update(block.epoch.to_le_bytes());
                hasher.update("PB1_FINISH")
            }
            Val::CommitVector(cv) => {
                hasher.update(cv.epoch.to_le_bytes());
                hasher.update("PB2_FINISH")
            }
        }
        Digest(hasher.finalize().as_slice()[..32].try_into().unwrap())
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct RandomnessShare {
    pub epoch: EpochNumber, // eopch
    pub view: ViewNumber,   // view
    pub author: PublicKey,
    pub signature_share: SignatureShare,
}

impl RandomnessShare {
    pub async fn new(
        epoch: EpochNumber,
        view: ViewNumber,
        author: PublicKey,
        mut signature_service: SignatureService,
    ) -> Self {
        let digest = digest!(epoch.to_le_bytes(), view.to_le_bytes(), "RANDOMNESS_SHARE");
        let signature_share = signature_service
            .request_tss_signature(digest)
            .await
            .unwrap();
        Self {
            epoch,
            view,
            author,
            signature_share,
        }
    }

    pub fn verify(
        &self,
        committee: &Committee,
        pk_set: &PublicKeySet,
        halt_mark: EpochNumber,
        epochs_halted: &HashSet<EpochNumber>,
    ) -> ConsensusResult<()> {
        // Check for epoch.
        ensure!(
            self.epoch > halt_mark && !epochs_halted.contains(&self.epoch),
            ConsensusError::MessageWithHaltedEpoch(self.epoch, halt_mark + 1)
        );

        // Ensure the authority has voting rights.
        ensure!(
            committee.stake(&self.author) > 0,
            ConsensusError::UnknownAuthority(self.author)
        );

        // Check the signature.
        let share = pk_set.public_key_share(committee.id(self.author));
        ensure!(
            share.verify(&self.signature_share, &self.digest()),
            ConsensusError::InvalidSignatureShare(self.author)
        );

        Ok(())
    }
}

impl Hash for RandomnessShare {
    fn digest(&self) -> Digest {
        digest!(
            self.epoch.to_le_bytes(),
            self.view.to_le_bytes(),
            "RANDOMNESS_SHARE"
        )
    }
}

impl fmt::Debug for RandomnessShare {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        write!(
            f,
            "RandomnessShare(author {}, epoch {}, view {})",
            self.author, self.epoch, self.view
        )
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct RandomCoin {
    pub author: PublicKey,
    pub epoch: EpochNumber,                         // epoch
    pub view: ViewNumber,                           // view
    pub leader: PublicKey,                          // elected leader of the view
    pub threshold_sig: threshold_crypto::Signature, // combined signature
}

impl RandomCoin {
    pub fn verify(
        &self,
        committee: &Committee,
        pk_set: &PublicKeySet,
        halt_mark: EpochNumber,
        epochs_halted: &HashSet<EpochNumber>,
    ) -> ConsensusResult<()> {
        // Check epoch.
        ensure!(
            self.epoch > halt_mark && !epochs_halted.contains(&self.epoch),
            ConsensusError::MessageWithHaltedEpoch(self.epoch, halt_mark + 1)
        );

        // Check threshold signature.
        let digest = digest!(
            self.epoch.to_le_bytes(),
            self.view.to_le_bytes(),
            "RANDOMNESS_SHARE"
        );
        ensure!(
            pk_set.public_key().verify(&self.threshold_sig, digest),
            ConsensusError::InvalidThresholdSignature(self.author)
        );

        // Check leader.
        let id = usize::from_be_bytes((&self.threshold_sig.to_bytes()[0..8]).try_into().unwrap())
            % committee.size();
        let mut keys: Vec<_> = committee.authorities.keys().cloned().collect();
        keys.sort();
        let leader = keys[id];
        ensure!(
            leader == self.leader,
            ConsensusError::RandomCoinWithWrongLeader
        );

        Ok(())
    }
}

impl fmt::Debug for RandomCoin {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        write!(
            f,
            "RandomCoin(epoch {}, view {}, leader {})",
            self.epoch, self.view, self.leader
        )
    }
}

impl Hash for RandomCoin {
    fn digest(&self) -> Digest {
        digest!(
            self.epoch.to_le_bytes(),
            self.view.to_le_bytes(),
            "RANDOM_COIN"
        )
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Done {
    pub author: PublicKey,
    pub coin: RandomCoin,
    pub proof: Sigma,
}

impl Done {
    pub fn verify(
        &self,
        committee: &Committee,
        pk_set: &PublicKeySet,
        halt_mark: EpochNumber,
        epochs_halted: &HashSet<EpochNumber>,
    ) -> ConsensusResult<()> {
        // Ensure the authority has voting rights.
        ensure!(
            committee.stake(&self.author) > 0,
            ConsensusError::UnknownAuthority(self.author)
        );

        // Check validity of random coin.
        self.coin
            .verify(committee, pk_set, halt_mark, epochs_halted)
    }

    pub fn check_sigma(&self, pk: &threshold_crypto::PublicKey) -> bool {
        if let Some(sigma) = &self.proof {
            return pk.verify(&sigma, self.digest());
        }
        false
    }
}

impl Hash for Done {
    fn digest(&self) -> Digest {
        digest!(
            self.coin.epoch.to_le_bytes(),
            self.coin.view.to_le_bytes(),
            "DONE"
        )
    }
}

impl fmt::Debug for Done {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        write!(
            f,
            "PreVote(author {}, epoch {}, view {}, vote {})",
            self.author,
            self.coin.epoch,
            self.coin.view,
            match &self.proof {
                Some(_) => "Yes",
                None => "No",
            }
        )
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Halt {
    pub block: Block,
    pub author: PublicKey,
}

impl Halt {
    pub fn verify(
        &self,
        committee: &Committee,
        pk_set: &PublicKeySet,
        halt_mark: EpochNumber,
        epochs_halted: &HashSet<EpochNumber>,
    ) -> ConsensusResult<()> {
        // Ensure the authority has voting rights.
        ensure!(
            committee.stake(&self.author) > 0,
            ConsensusError::UnknownAuthority(self.author)
        );
        
        // Verify block.
        self.block.verify(committee, halt_mark, epochs_halted)?;
        ensure!(
            self.block.check_sigma(&pk_set.public_key()),
            ConsensusError::InvalidSignatureShare(self.block.author)
        );
        Ok(())
    }
}

impl Hash for Halt {
    fn digest(&self) -> Digest {
        // Echo is distinguished by <epoch, phase>,
        digest!(
            self.block.epoch.to_le_bytes(),
            self.block.author.0,
            "HALT"
        )
    }
}