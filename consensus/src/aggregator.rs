use crate::config::{Committee, Stake};
use crate::error::{ConsensusError, ConsensusResult};
use crate::messages::ConsensusMessage;
use crypto::PublicKey;
use std::collections::HashSet;

#[cfg(test)]
#[path = "tests/aggregator_tests.rs"]
pub mod aggregator_tests;

pub struct Aggregator {
    pub weight: Stake,
    pub votes: Vec<ConsensusMessage>,
    pub used: HashSet<PublicKey>,
}

impl Aggregator {
    pub fn new() -> Self {
        Self {
            weight: 0,
            votes: Vec::new(),
            used: HashSet::new(),
        }
    }

    pub fn append(&mut self, author: PublicKey, vote: ConsensusMessage, committee: &Committee) -> ConsensusResult<Option<Vec<ConsensusMessage>>> {
        // Ensure it is the first time this authority votes.
        ensure!(
            self.used.insert(author),
            ConsensusError::AuthorityReuseinQC(author)
        );
        self.votes.push(vote);
        self.weight += committee.stake(&author);

        let threshold = match vote {
            ConsensusMessage::RandomnessShare(_) => committee.random_coin_threshold(),
            _ => committee.quorum_threshold(),
        };
        if self.weight >= threshold {
            self.weight = 0; // Ensures QC is only made once.
            return Ok(Some(self.votes));
        }
        Ok(None)
    }

    // To see if votes meet random coin threshold.
    pub fn ready_for_random_coin(&self, committee: &Committee) -> bool {
        self.weight == committee.random_coin_threshold()
    }
}