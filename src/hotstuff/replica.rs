use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, RwLock},
};

use ed25519_dalek::{SigningKey, VerifyingKey};
use tokio::{
    pin,
    sync::mpsc::{self},
    time::sleep,
};

use crate::{
    config,
    hotstuff::{message::Phase, utils},
    node::client::handler::QueryRequest,
    replica_debug, replica_log,
    state::state::LedgerState,
    types::{
        consensus::ViewNumber,
        message::{ReplicaInBound, ReplicaOutbound},
        transaction::{PublicKeyHash, Sha256Hash, SignedTransaction},
    },
};

use super::{
    block::{Block, BlockHash},
    crypto::{PartialSig, QuorumCertificate},
    mempool::PriorityMempool,
    message::HotStuffMessage,
    message_window::MessageWindow,
    pacemaker::Pacemaker,
    replica_sender::ReplicaSender,
};

const BLOCK_TRANSACTION_LENGTH: usize = 16;

struct ViewProgress {
    pub leader_has_proposed: bool,
    pub replica_has_voted: bool,
}

impl ViewProgress {
    fn reset(&mut self) {
        self.leader_has_proposed = false;
        self.replica_has_voted = false;
    }
}

pub struct HotStuffReplica {
    pub node_id: usize,
    pub validator_set: HashSet<VerifyingKey>,
    signing_key: SigningKey,

    // Arc is needed as we are sending our replica across threads
    generic_qc: Arc<QuorumCertificate>,
    locked_qc: Arc<QuorumCertificate>,

    current_proposal: Option<Arc<RwLock<Block>>>,
    mempool: PriorityMempool,
    pending_transactions: HashMap<Sha256Hash, SignedTransaction>,
    committed_transactions: HashMap<Sha256Hash, SignedTransaction>,

    pub messages: MessageWindow,
    pub pacemaker: Pacemaker,

    pub rep_node_channel: ReplicaSender,

    // State
    blockstore: HashMap<BlockHash, Arc<RwLock<Block>>>,
    ledger_state: LedgerState,

    view_progress: ViewProgress,
}

impl HotStuffReplica {
    pub fn new(
        node_id: usize,
        replica_tx: mpsc::Sender<ReplicaInBound>,
        node_tx: mpsc::Sender<ReplicaOutbound>,
    ) -> Self {
        let signing_key = config::retrieve_signing_key_checked(node_id);

        let (genesis_block, genesis_qc) = Block::create_genesis_block();
        let mut blockstore: HashMap<BlockHash, Arc<RwLock<Block>>> = HashMap::new();
        blockstore.insert(
            genesis_block.hash(),
            Arc::new(RwLock::new(genesis_block.clone())),
        );

        let genesis_qc = Arc::new(genesis_qc);
        HotStuffReplica {
            node_id,
            validator_set: config::retrieve_validator_set(),
            signing_key,

            generic_qc: genesis_qc.clone(),
            locked_qc: genesis_qc,

            current_proposal: None,
            blockstore,
            mempool: PriorityMempool::new(),
            pending_transactions: HashMap::new(),
            committed_transactions: HashMap::new(),

            // TODO: pass in config
            messages: MessageWindow::new(0, 100),

            pacemaker: Pacemaker::new(),
            rep_node_channel: ReplicaSender {
                replica_tx,
                node_tx,
            },

            ledger_state: LedgerState::new(),

            view_progress: ViewProgress {
                leader_has_proposed: false,
                replica_has_voted: false,
            },
        }
    }

    pub fn vote_message(&mut self, node: &Block) -> HotStuffMessage {
        HotStuffMessage::create_vote(
            node.clone(), // need to clone as we are serialising our message
            self.pacemaker.curr_view,
            self.node_id,
            self.pacemaker.curr_view,
            &mut self.signing_key,
        )
    }

    pub fn matching_message(message: HotStuffMessage, view_number: ViewNumber) -> bool {
        view_number == message.get_view_number()
    }

    fn quorum_threshold(&self) -> usize {
        let n = self.validator_set.len();
        2 * ((n - 1) / 3) + 1
    }

    fn validate_block_transactions(node: &Block) -> bool {
        node.transactions()
            .iter()
            .all(|transaction| transaction.verify_sender())
    }

    /// Try to build QC(view) once any block‐hash has n‑f signatures.
    /// Returns None if no such QC exists yet.
    pub fn try_create_qc_for_view(&self, view: ViewNumber) -> Option<QuorumCertificate> {
        let vote_msgs = &self
            .messages
            .get_phase_messages_for_view(view, Phase::Vote)?;

        // 2) Tally signatures by (block_hash → Vec<PartialSig>),
        //    verifying and deduplicating by signer.
        let mut seen: HashSet<PublicKeyHash> = HashSet::new();
        let mut tally: HashMap<(BlockHash, Sha256Hash), Vec<&PartialSig>> = HashMap::new();

        for message in vote_msgs {
            if let HotStuffMessage::Vote {
                partial_sig, node, ..
            } = message
            {
                // a) signature must come from a known, unseen validator
                if !self.validator_set.contains(&partial_sig.signer_id) {
                    continue;
                }

                let msg_hash = &message.hash();

                if partial_sig
                    .signer_id
                    .verify_strict(msg_hash, &partial_sig.signature)
                    .is_err()
                {
                    continue;
                }

                if !seen.insert(*partial_sig.signer_id.as_bytes()) {
                    continue;
                }

                // b) extract the vote’s block hash
                let blockhash = node.hash();

                // ensure that quorum has same msg_hash
                tally
                    .entry((blockhash, *msg_hash))
                    .or_default()
                    .push(partial_sig);
            }
        }

        // 3) As soon as any bucket reaches quorum, build the QC and return
        let quorum = self.quorum_threshold();
        for ((block_hash, message_hash), sigs) in tally {
            if sigs.len() >= quorum {
                return Some(QuorumCertificate::from_signatures(
                    view,
                    block_hash,
                    message_hash,
                    sigs,
                ));
            }
        }

        None
    }

    pub fn safe_node(&self, block: &Block, qc: &QuorumCertificate) -> bool {
        let locked_qc = &self.locked_qc;
        let locked_block_hash = locked_qc.block_hash;
        let extends = block.extends_from(locked_block_hash, &self.blockstore);
        let newer_qc = qc.view_number > locked_qc.view_number;

        extends || newer_qc
    }

    /// Selects a transaction from the mempool
    fn select_transactions(&mut self) -> Vec<SignedTransaction> {
        self.mempool.pop_next_n(BLOCK_TRANSACTION_LENGTH)
    }

    fn leader_create_message(&mut self, new_block: Block) -> HotStuffMessage {
        HotStuffMessage::create_proposal(
            new_block,
            self.pacemaker.curr_view,
            self.node_id,
            self.pacemaker.curr_view,
        )
    }

    fn get_justified_block(&self, block: &Block) -> Option<Arc<RwLock<Block>>> {
        let hash = match block {
            Block::Normal { justify, .. } => &justify.block_hash,
            Block::Genesis { .. } => return None,
        };
        self.blockstore.get(hash).cloned()
    }

    fn get_justifed_block_and_qc(
        &self,
        block: Arc<RwLock<Block>>,
    ) -> (Option<Arc<RwLock<Block>>>, Option<QuorumCertificate>) {
        let binding = block.read().unwrap();
        let justify = match &*binding {
            Block::Normal { justify, .. } => justify,
            Block::Genesis { .. } => return (None, None),
        };

        let child_block = self.blockstore.get(&justify.block_hash).cloned();
        (child_block, Some(justify.clone()))
    }

    fn is_parent(&self, block_child: &Block, block_parent: &Block) -> bool {
        match *block_child {
            Block::Genesis { .. } => false,
            Block::Normal { parent_id, .. } => block_parent.hash() == parent_id,
        }
    }
    fn add_block_transactions_to_pending(&mut self, block: &Block) {
        let transactions = block.transactions();

        for transaction in transactions.iter() {
            self.pending_transactions
                .insert(transaction.hash, transaction.clone());
        }
    }

    fn add_block_transactions_to_committed(&mut self, block: &Block) {
        let transactions = block.transactions();

        for transaction in transactions.iter() {
            self.committed_transactions
                .insert(transaction.hash, transaction.clone());
        }
    }

    fn remove_block_transactions_from_pending(&mut self, block: &Block) {
        let transactions = block.transactions();

        for transaction in transactions.iter() {
            self.pending_transactions.remove_entry(&transaction.hash);
        }
    }

    pub fn leader_handle_message(&mut self) -> Option<HotStuffMessage> {
        let curr_view = self.pacemaker.curr_view;
        replica_log!(
            self.node_id,
            "Leader handle message at view: {:?}",
            curr_view
        );

        self.pacemaker.reset_timer();

        let view_messages = self
            .messages
            .get_phase_messages_for_view(curr_view, Phase::NewView);

        if let Some(msgs) = view_messages
            && msgs.len() >= self.quorum_threshold()
        {
            replica_log!(
                self.node_id,
                "Leader handle new view (Prepare), view num: {:?}",
                self.pacemaker.curr_view
            );

            if self.generic_qc.view_number != curr_view - 1 {
                // msgs should only contain justify if next-view interupt is triggered
                if let Some(high_qc) = utils::get_highest_qc_from_votes(&msgs)
                    && high_qc.view_number > self.generic_qc.view_number
                {
                    // update generic qc if replica falls behind
                    self.generic_qc = Arc::new(high_qc.clone());
                    self.messages.prune_before_view(self.generic_qc.view_number);
                }
            }

            let parent = {
                let Some(parent) = self.blockstore.get(&self.generic_qc.block_hash) else {
                    // cant find QC's block
                    // replica_debug!(
                    //     self.node_id,
                    //     self.pacemaker.curr_view,
                    //     "cant find qc's block: L"
                    // );
                    return None;
                };
                parent.clone()
            };

            let selected_transactions = &self.select_transactions();

            let curr_view = self.pacemaker.curr_view;
            let new_block = Block::create_leaf(
                &parent.read().unwrap(),
                selected_transactions.clone(),
                curr_view,
                (*self.generic_qc).clone(),
            );

            let sending_block = new_block.clone();
            let new_block = Arc::new(RwLock::new(new_block));
            let new_block_hash = new_block.read().unwrap().hash();
            self.blockstore.insert(new_block_hash, new_block.clone());

            self.current_proposal = Some(new_block);
            // replica_debug!(
            //     self.node_id,
            //     self.pacemaker.curr_view,
            //     "Success: Leader:: Prepare"
            // );

            // leader should also vote for their own message
            return Some(self.leader_create_message(sending_block));
        }
        // is not new view

        let has_quorum_votes = utils::has_quorum_votes_for_view(
            self.messages
                .get_phase_messages_for_view(curr_view - 1, Phase::Vote),
            self.pacemaker.curr_view - 1,
            self.quorum_threshold(),
        );

        if has_quorum_votes {
            // Attempt to form QC from votes from prev view

            let qc = self.try_create_qc_for_view(curr_view - 1);

            match qc {
                Some(qc) => {
                    self.generic_qc = Arc::new(qc);
                    self.messages.prune_before_view(self.generic_qc.view_number);
                    // replica_debug!(self.node_id, self.pacemaker.curr_view, "Able to form QC");
                }

                None => {
                    // Unable to form a QC, continue propose new block justified from highest qc or generic qc
                    // replica_debug!(self.node_id, self.pacemaker.curr_view, "Unable to form QC");
                    return None;
                }
            }
        }

        // replica_debug!(
        //     self.node_id,
        //     self.pacemaker.curr_view,
        //     "waiting for quorum (n - f)",
        // );
        None
    }

    pub fn replica_handle_proposal(
        &mut self,
        node: Block,
        sender: usize,
    ) -> Option<HotStuffMessage> {
        let curr_view = self.pacemaker.curr_view;
        if sender != self.pacemaker.get_leader_for_view(curr_view) {
            // Ignore messages not from leader of the current view
            // replica_debug!(
            //     self.node_id,
            //     self.pacemaker.curr_view,
            //     "msg sender: {:?} is not the leader. Leader is: {:?}",
            //     msg.sender,
            //     self.pacemaker.get_leader_for_view(curr_view)
            // );
            return None;
        }
        // b*
        let b_star = Arc::new(RwLock::new(node));
        self.blockstore
            .insert(b_star.read().unwrap().hash(), b_star.clone());

        // b″ := b*.justify.node
        let (Some(b_double_prime), Some(b_star_justify)) =
            self.get_justifed_block_and_qc(b_star.clone())
        else {
            // replica_debug!(
            //     self.node_id,
            //     self.pacemaker.curr_view,
            //     "Missing b″ justified by b* or missing justify on b*"
            // );
            return None;
        };

        let b_star_justify = Arc::new(b_star_justify.clone());

        let mut outbound_msg = None;

        let is_safe = {
            // This scope ends after the function call
            self.safe_node(&b_star.read().unwrap(), &b_star_justify)
        };

        let is_valid_sig = { b_star_justify.verify(&self.validator_set, self.quorum_threshold()) };

        if is_safe && is_valid_sig {
            let block = b_star.read().unwrap();

            if !Self::validate_block_transactions(&block) {
                return None;
            }

            let block_merkle_root = block.hash_block_transaction();
            if block_merkle_root != block.merkle_root() {
                return None;
            }
            outbound_msg = Some(self.vote_message(&b_star.read().unwrap()));
            self.add_block_transactions_to_pending(&b_star.read().unwrap());
            // replica_debug!(
            //     self.node_id,
            //     self.pacemaker.curr_view,
            //     "voting for block, b*: {:?}",
            //     b_star.transactions()
            // );
        } else {
            return outbound_msg;
        }

        if !self.is_parent(&b_star.read().unwrap(), &b_double_prime.read().unwrap()) {
            return outbound_msg;
        }
        self.generic_qc = b_star_justify.clone();

        self.messages.prune_before_view(self.generic_qc.view_number);

        // replica_debug!(
        //     self.node_id,
        //     self.pacemaker.curr_view,
        //     "Start commit phase on b*'s grandparent",
        // );

        // b′ := b″.justify.node
        let (Some(b_prime), Some(b_double_prime_justify)) =
            self.get_justifed_block_and_qc(b_double_prime.clone())
        else {
            // replica_debug!(
            //     self.node_id,
            //     self.pacemaker.curr_view,
            //     "Missing b′ justified by b″"
            // );
            return outbound_msg;
        };

        let b_double_prime_justify = Arc::new(b_double_prime_justify.clone());

        if !self.is_parent(&b_double_prime.read().unwrap(), &b_prime.read().unwrap()) {
            return outbound_msg;
        }

        self.locked_qc = b_double_prime_justify.clone();
        self.pacemaker
            .set_last_committed_view(self.locked_qc.clone());

        // b := b′.justify.node
        let commited_block = {
            let Some(b) = self.get_justified_block(&b_prime.read().unwrap()) else {
                // replica_debug!(
                //     self.node_id,
                //     self.pacemaker.curr_view,
                //     "Missing b justified by b′"
                // );
                return outbound_msg;
            };

            if !self.is_parent(&b_prime.read().unwrap(), &b.read().unwrap()) {
                return outbound_msg;
            }
            b
        };

        replica_log!(self.node_id, "Commit success on view: {:?}", curr_view);
        // replica_debug!(
        //     self.node_id,
        //     self.pacemaker.curr_view,
        //     "Applying transaction, {:?}",
        //     &commited_block.transactions()
        // );
        let account_nonces = self
            .ledger_state
            .apply_block(&mut commited_block.write().unwrap());
        self.remove_block_transactions_from_pending(&commited_block.read().unwrap());
        self.add_block_transactions_to_committed(&commited_block.read().unwrap());
        self.mempool.update_after_execution(account_nonces);

        outbound_msg
    }

    pub fn replica_handle_vote(&mut self) -> Option<HotStuffMessage> {
        let curr_view = self.pacemaker.curr_view;
        if self.node_id != self.pacemaker.get_leader_for_view(curr_view + 1) {
            return None;
        }

        if !self.view_progress.replica_has_voted {
            // Node must vote before attempting to lead the next view, else it's own vote would be missing from the quorum
            return None;
        }

        if !utils::has_quorum_votes_for_view(
            self.messages
                .get_phase_messages_for_view(curr_view, Phase::Vote),
            curr_view,
            self.quorum_threshold(),
        ) {
            return None;
        }

        replica_debug!(
            self.node_id,
            self.pacemaker.curr_view,
            "Optimistically advancing to new view",
        );
        // Advance view early without waiting for pacemaker timeout
        self.pacemaker.advance_view();

        Some(self.create_new_view())
    }

    pub fn replica_handle_message(&mut self, msg: HotStuffMessage) -> Option<HotStuffMessage> {
        // replica_log!(
        //     self.node_id,
        //     "Replica handle message with at view: {:?}",
        //     self.pacemaker.curr_view
        // );

        match msg {
            HotStuffMessage::NewView { .. } => {
                // replica shouldn't handle new view
                None
            }
            HotStuffMessage::Proposal { node, sender, .. } => {
                self.replica_handle_proposal(node, sender)
            }
            HotStuffMessage::Vote { .. } => {
                // Messages that fall in this block are votes sent to the *next* leader
                // We can try to optimistically advance the view, otherwise we ignore the messages
                self.replica_handle_vote()
            }
        }
    }

    fn sync_view(&mut self, msg: &HotStuffMessage) -> bool {
        let incoming_view = msg.get_view_number();
        self.pacemaker.fast_forward_view(incoming_view)
    }

    async fn handle_message(&mut self, msg: HotStuffMessage) -> Result<(), std::io::Error> {
        if self.sync_view(&msg) {
            // replica_debug!(
            //     self.node_id,
            //     self.pacemaker.curr_view,
            //     "Recieved higher view message from node: {:?}",
            //     msg.sender
            // );
            self.view_progress.reset();
            // advance view if view is behind
            let is_leader = self.pacemaker.current_leader() == self.node_id;
            if is_leader {
                // Send new-view msg to self
                let new_view_msg: HotStuffMessage = self.create_new_view();
                self.rep_node_channel.send_to_self(new_view_msg).await?
            }
        }

        if msg.get_view_number() + 1 < self.pacemaker.curr_view {
            // replica_debug!(
            //     self.node_id,
            //     self.pacemaker.curr_view,
            //     "Recieved stale message from view: {:?} from node: {:?}",
            //     msg.view_number,
            //     msg.sender
            // );
        }

        self.messages.push(msg.clone());

        let leader = self.pacemaker.current_leader();
        let is_leader = self.node_id == leader;
        let curr_view = self.pacemaker.curr_view;

        // Choose which role-specific handler to run:
        // Every node executes replica logic
        // Leader executes leader logic and sends message to itself to handle as replica
        if is_leader && !self.view_progress.leader_has_proposed {
            let leader_outbound_msg_opt = self.leader_handle_message();
            let Some(leader_outbound_msg) = leader_outbound_msg_opt else {
                return Ok(());
            };

            self.rep_node_channel
                .broadcast(leader_outbound_msg.clone())
                .await?;
            self.view_progress.leader_has_proposed = true;

            // handle leader's msg as replica
            // Dont send to same channel to prevent data race
            self.messages.push(leader_outbound_msg.clone());
            let replica_outbound_msg_opt = self.replica_handle_message(leader_outbound_msg.clone());

            let Some(replica_outbound_msg) = replica_outbound_msg_opt else {
                return Ok(());
            };
            let next_leader = self
                .pacemaker
                .get_leader_for_view(self.pacemaker.curr_view + 1);

            // replica_debug!(
            //     self.node_id,
            //     self.pacemaker.curr_view,
            //     "Sending to next leader: {:?} with msg view: {:?}",
            //     next_leader,
            //     replica_outbound_msg.view_number
            // );

            self.rep_node_channel
                .send_to_node(next_leader, replica_outbound_msg)
                .await?;

            self.view_progress.replica_has_voted = true;

            return Ok(());
        }

        // Handle as replica only
        let outbound_msg_opt = self.replica_handle_message(msg);

        let Some(outbound_msg) = outbound_msg_opt else {
            return Ok(());
        };

        let next_leader = self.pacemaker.get_leader_for_view(curr_view + 1);

        self.view_progress.replica_has_voted = true;

        if self.node_id == next_leader {
            // replica_debug!(
            //     self.node_id,
            //     self.pacemaker.curr_view,
            //     "Sending to self as node is next leader"
            // );
            let _ = self.rep_node_channel.send_to_self(outbound_msg).await;

            return Ok(());
        }

        // replica_debug!(
        //     self.node_id,
        //     self.pacemaker.curr_view,
        //     "Sending to next leader: {:?}",
        //     next_leader,
        // );

        self.rep_node_channel
            .send_to_node(next_leader, outbound_msg)
            .await?;
        Ok(())
    }

    fn create_new_view(&mut self) -> HotStuffMessage {
        HotStuffMessage::create_new_view(
            (*self.generic_qc).clone(),
            self.pacemaker.curr_view - 1,
            self.node_id,
            self.pacemaker.curr_view - 1,
        )
    }

    fn handle_query(&self, query_request: QueryRequest) {
        let client_response = self.ledger_state.handle_query(query_request.query);
        let _ = query_request.response_channel.send(client_response);
    }

    fn handle_transaction(&mut self, txn: SignedTransaction) {
        let account_info = self
            .ledger_state
            .get_account_info_or_default(&txn.get_from_account());
        self.mempool.insert(txn, account_info.expected_nonce);
    }

    async fn send_new_view_to_leader(&mut self) -> Result<(), std::io::Error> {
        let leader = self.pacemaker.current_leader();
        let outbound_msg = self.create_new_view();

        if self.node_id == leader {
            self.rep_node_channel.send_to_self(outbound_msg).await
            // replica_debug!(self.node_id, self.pacemaker.curr_view -1 , "Timeout: Sending self");
        } else {
            // replica_debug!(self.node_id, self.pacemaker.curr_view -1 , "Timeout: Sending to node: {:?} with msg view: {:?}", leader, outbound_msg.view_number);
            self.rep_node_channel
                .send_to_node(leader, outbound_msg)
                .await
        }
    }

    async fn advance_view(&mut self) -> Result<(), std::io::Error> {
        self.pacemaker.advance_view();
        self.view_progress.reset();
        self.send_new_view_to_leader().await?;
        Ok(())
    }

    pub async fn run_replica(
        &mut self,
        mut to_replica_rx: mpsc::Receiver<ReplicaInBound>,
    ) -> Result<(), std::io::Error> {
        replica_log!(self.node_id, "Running replica...");
        loop {
            // Refresh pacemaker timer dynamically each loop
            let time_remaining = self.pacemaker.time_remaining();
            let pacemaker_timer = sleep(time_remaining);
            pin!(pacemaker_timer);

            tokio::select! {
                Some(msg) = to_replica_rx.recv() => {
                    match msg {
                        ReplicaInBound::HotStuff(msg) => self.handle_message(msg).await?,
                        ReplicaInBound::Transaction(tx) => self.handle_transaction(tx),
                        ReplicaInBound::Query(query) => self.handle_query(query),
                    }
                },

                _ = &mut pacemaker_timer => {
                    if self.pacemaker.should_advance_view() {
                        self.advance_view().await?;
                    }
                }
            }
        }
    }
}
