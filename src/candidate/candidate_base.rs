use super::*;
use crate::errors::*;
use crate::util::*;

use stun::message::*;

use async_trait::async_trait;
use crc::{Crc, CRC_32_ISCSI};
use std::fmt;
use std::ops::Add;
use std::sync::atomic::{AtomicU16, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{broadcast, Mutex};

#[derive(Default)]
pub struct CandidateBaseConfig {
    pub candidate_id: String,
    pub network: String,
    pub address: String,
    pub port: u16,
    pub component: u16,
    pub priority: u32,
    pub foundation: String,
    pub conn: Option<Arc<dyn util::Conn + Send + Sync>>,
    pub initialized_ch: Option<broadcast::Receiver<()>>,
}

pub(crate) type OnClose = fn() -> Result<(), Error>;

pub struct CandidateBase {
    pub(crate) id: String,
    pub(crate) network_type: AtomicU8,
    pub(crate) candidate_type: CandidateType,

    pub(crate) component: AtomicU16,
    pub(crate) address: String,
    pub(crate) port: u16,
    pub(crate) related_address: Option<CandidateRelatedAddress>,
    pub(crate) tcp_type: TcpType,

    pub(crate) resolved_addr: Mutex<SocketAddr>,

    pub(crate) last_sent: AtomicU64,
    pub(crate) last_received: AtomicU64,

    pub(crate) conn: Option<Arc<dyn util::Conn + Send + Sync>>,
    pub(crate) agent_internal: Option<Arc<Mutex<AgentInternal>>>,
    pub(crate) closed_ch: Arc<Mutex<Option<broadcast::Sender<()>>>>,

    pub(crate) foundation_override: String,
    pub(crate) priority_override: u32,

    //CandidateHost
    pub(crate) network: String,
    //CandidateRelay
    pub(crate) relay_client: Option<Arc<turn::client::Client>>,
}

impl Default for CandidateBase {
    fn default() -> Self {
        Self {
            id: String::new(),
            network_type: AtomicU8::new(0),
            candidate_type: CandidateType::default(),

            component: AtomicU16::new(0),
            address: String::new(),
            port: 0,
            related_address: None,
            tcp_type: TcpType::default(),

            resolved_addr: Mutex::new(SocketAddr::new(IpAddr::from([0, 0, 0, 0]), 0)),

            last_sent: AtomicU64::new(0),
            last_received: AtomicU64::new(0),

            conn: None,
            agent_internal: None,
            closed_ch: Arc::new(Mutex::new(None)),

            foundation_override: String::new(),
            priority_override: 0,
            network: String::new(),
            relay_client: None,
        }
    }
}

// String makes the candidateBase printable
impl fmt::Display for CandidateBase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(related_address) = self.related_address() {
            write!(
                f,
                "{} {} {}:{}{}",
                self.network_type(),
                self.candidate_type(),
                self.address(),
                self.port(),
                related_address,
            )
        } else {
            write!(
                f,
                "{} {} {}:{}",
                self.network_type(),
                self.candidate_type(),
                self.address(),
                self.port(),
            )
        }
    }
}

#[async_trait]
impl Candidate for CandidateBase {
    fn foundation(&self) -> String {
        if !self.foundation_override.is_empty() {
            return self.foundation_override.clone();
        }

        let mut buf = vec![];
        buf.extend_from_slice(self.candidate_type().to_string().as_bytes());
        buf.extend_from_slice(self.address.as_bytes());
        buf.extend_from_slice(self.network_type().to_string().as_bytes());

        let checksum = Crc::<u32>::new(&CRC_32_ISCSI).checksum(&buf);

        format!("{}", checksum)
    }

    /// Returns Candidate ID.
    fn id(&self) -> String {
        self.id.clone()
    }

    /// Returns candidate component.
    fn component(&self) -> u16 {
        self.component.load(Ordering::SeqCst)
    }

    fn set_component(&self, component: u16) {
        self.component.store(component, Ordering::SeqCst);
    }

    /// Returns a time indicating the last time this candidate was received.
    fn last_received(&self) -> SystemTime {
        UNIX_EPOCH.add(Duration::from_nanos(
            self.last_received.load(Ordering::SeqCst),
        ))
    }

    /// Returns a time indicating the last time this candidate was sent.
    fn last_sent(&self) -> SystemTime {
        UNIX_EPOCH.add(Duration::from_nanos(self.last_sent.load(Ordering::SeqCst)))
    }

    /// Returns candidate NetworkType.
    fn network_type(&self) -> NetworkType {
        NetworkType::from(self.network_type.load(Ordering::SeqCst))
    }

    /// Returns Candidate Address.
    fn address(&self) -> String {
        self.address.clone()
    }

    /// Returns Candidate Port.
    fn port(&self) -> u16 {
        self.port
    }

    /// Computes the priority for this ICE Candidate.
    fn priority(&self) -> u32 {
        if self.priority_override != 0 {
            return self.priority_override;
        }

        // The local preference MUST be an integer from 0 (lowest preference) to
        // 65535 (highest preference) inclusive.  When there is only a single IP
        // address, this value SHOULD be set to 65535.  If there are multiple
        // candidates for a particular component for a particular data stream
        // that have the same type, the local preference MUST be unique for each
        // one.
        (1 << 24) * u32::from(self.candidate_type().preference())
            + (1 << 8) * u32::from(self.local_preference())
            + (256 - u32::from(self.component()))
    }

    /// Returns `Option<CandidateRelatedAddress>`.
    fn related_address(&self) -> Option<CandidateRelatedAddress> {
        self.related_address.as_ref().cloned()
    }

    /// Returns candidate type.
    fn candidate_type(&self) -> CandidateType {
        self.candidate_type
    }

    fn tcp_type(&self) -> TcpType {
        self.tcp_type
    }

    /// Returns the string representation of the ICECandidate.
    fn marshal(&self) -> String {
        let mut val = format!(
            "{} {} {} {} {} {} typ {}",
            self.foundation(),
            self.component(),
            self.network_type().network_short(),
            self.priority(),
            self.address(),
            self.port(),
            self.candidate_type()
        );

        if self.tcp_type != TcpType::Unspecified {
            val += format!(" tcptype {}", self.tcp_type()).as_str();
        }

        if let Some(related_address) = self.related_address() {
            val += format!(
                " raddr {} rport {}",
                related_address.address, related_address.port,
            )
            .as_str();
        }

        val
    }

    async fn addr(&self) -> SocketAddr {
        let resolved_addr = self.resolved_addr.lock().await;
        *resolved_addr
    }

    /// Stops the recvLoop.
    async fn close(&self) -> Result<(), Error> {
        {
            let mut closed_ch = self.closed_ch.lock().await;
            if closed_ch.is_none() {
                return Err(ERR_CLOSED.to_owned());
            }
            closed_ch.take();
        }

        if let Some(relay_client) = &self.relay_client {
            relay_client.close().await
        } else {
            Ok(())
        }
    }

    fn seen(&self, outbound: bool) {
        let d = match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(d) => d,
            Err(_) => Duration::from_secs(0),
        };

        if outbound {
            self.set_last_sent(d);
        } else {
            self.set_last_received(d);
        }
    }

    async fn write_to(
        &self,
        raw: &[u8],
        dst: &(dyn Candidate + Send + Sync),
    ) -> Result<usize, Error> {
        let n = if let Some(conn) = &self.conn {
            let addr = dst.addr().await;
            conn.send_to(raw, addr).await?
        } else {
            0
        };
        self.seen(true);
        Ok(n)
    }

    /// Used to compare two candidateBases.
    fn equal(&self, other: &dyn Candidate) -> bool {
        self.network_type() == other.network_type()
            && self.candidate_type() == other.candidate_type()
            && self.address() == other.address()
            && self.port() == other.port()
            && self.tcp_type() == other.tcp_type()
            && self.related_address() == other.related_address()
    }

    async fn set_ip(&self, ip: &IpAddr) -> Result<(), Error> {
        let network_type = determine_network_type(&self.network, ip)?;

        self.network_type
            .store(network_type as u8, Ordering::SeqCst);

        let mut resolved_addr = self.resolved_addr.lock().await;
        *resolved_addr = create_addr(network_type, *ip, self.port);

        Ok(())
    }

    fn get_conn(&self) -> Option<&Arc<dyn util::Conn + Send + Sync>> {
        self.conn.as_ref()
    }

    fn get_agent(&self) -> Option<&Arc<Mutex<AgentInternal>>> {
        self.agent_internal.as_ref()
    }

    fn get_closed_ch(&self) -> Arc<Mutex<Option<broadcast::Sender<()>>>> {
        self.closed_ch.clone()
    }
}

impl CandidateBase {
    pub fn set_last_received(&self, d: Duration) {
        #[allow(clippy::cast_possible_truncation)]
        self.last_received
            .store(d.as_nanos() as u64, Ordering::SeqCst);
    }

    pub fn set_last_sent(&self, d: Duration) {
        #[allow(clippy::cast_possible_truncation)]
        self.last_sent.store(d.as_nanos() as u64, Ordering::SeqCst);
    }

    /// Returns the local preference for this candidate.
    pub fn local_preference(&self) -> u16 {
        if self.network_type().is_tcp() {
            // RFC 6544, section 4.2
            //
            // In Section 4.1.2.1 of [RFC5245], a recommended formula for UDP ICE
            // candidate prioritization is defined.  For TCP candidates, the same
            // formula and candidate type preferences SHOULD be used, and the
            // RECOMMENDED type preferences for the new candidate types defined in
            // this document (see Section 5) are 105 for NAT-assisted candidates and
            // 75 for UDP-tunneled candidates.
            //
            // (...)
            //
            // With TCP candidates, the local preference part of the recommended
            // priority formula is updated to also include the directionality
            // (active, passive, or simultaneous-open) of the TCP connection.  The
            // RECOMMENDED local preference is then defined as:
            //
            //     local preference = (2^13) * direction-pref + other-pref
            //
            // The direction-pref MUST be between 0 and 7 (both inclusive), with 7
            // being the most preferred.  The other-pref MUST be between 0 and 8191
            // (both inclusive), with 8191 being the most preferred.  It is
            // RECOMMENDED that the host, UDP-tunneled, and relayed TCP candidates
            // have the direction-pref assigned as follows: 6 for active, 4 for
            // passive, and 2 for S-O.  For the NAT-assisted and server reflexive
            // candidates, the RECOMMENDED values are: 6 for S-O, 4 for active, and
            // 2 for passive.
            //
            // (...)
            //
            // If any two candidates have the same type-preference and direction-
            // pref, they MUST have a unique other-pref.  With this specification,
            // this usually only happens with multi-homed hosts, in which case
            // other-pref is the preference for the particular IP address from which
            // the candidate was obtained.  When there is only a single IP address,
            // this value SHOULD be set to the maximum allowed value (8191).
            let other_pref: u16 = 8191;

            let direction_pref: u16 = match self.candidate_type() {
                CandidateType::Host | CandidateType::Relay => match self.tcp_type() {
                    TcpType::Active => 6,
                    TcpType::Passive => 4,
                    TcpType::SimultaneousOpen => 2,
                    TcpType::Unspecified => 0,
                },
                CandidateType::PeerReflexive | CandidateType::ServerReflexive => {
                    match self.tcp_type() {
                        TcpType::SimultaneousOpen => 6,
                        TcpType::Active => 4,
                        TcpType::Passive => 2,
                        TcpType::Unspecified => 0,
                    }
                }
                CandidateType::Unspecified => 0,
            };

            (1 << 13) * direction_pref + other_pref
        } else {
            DEFAULT_LOCAL_PREFERENCE
        }
    }

    pub(crate) async fn recv_loop(
        candidate: Arc<dyn Candidate + Send + Sync>,
        agent_internal: Arc<Mutex<AgentInternal>>,
        mut closed_ch_rx: broadcast::Receiver<()>,
        initialized_ch: Option<broadcast::Receiver<()>>,
        conn: Arc<dyn util::Conn + Send + Sync>,
        addr: SocketAddr,
    ) -> Result<(), Error> {
        if let Some(mut initialized_ch) = initialized_ch {
            tokio::select! {
                _ = initialized_ch.recv() => {}
                _ = closed_ch_rx.recv() => return Err(ERR_CLOSED.to_owned()),
            }
        }

        let mut buffer = vec![0_u8; RECEIVE_MTU];
        let mut n;
        let mut src_addr;
        loop {
            tokio::select! {
               result = conn.recv_from(&mut buffer) => {
                   match result {
                       Ok((num, src)) => {
                            n = num;
                            src_addr = src;
                       }
                       Err(err) => return Err(Error::new(err.to_string())),
                   }
               },
                _  = closed_ch_rx.recv() => return Err(ERR_CLOSED.to_owned()),
            }

            Self::handle_inbound_candidate_msg(
                &candidate,
                &agent_internal,
                &buffer[..n],
                src_addr,
                addr,
            )
            .await;
        }
    }

    async fn handle_inbound_candidate_msg(
        c: &Arc<dyn Candidate + Send + Sync>,
        agent_internal: &Arc<Mutex<AgentInternal>>,
        buf: &[u8],
        src_addr: SocketAddr,
        addr: SocketAddr,
    ) {
        if stun::message::is_message(buf) {
            let mut m = Message {
                raw: vec![],
                ..Message::default()
            };
            // Explicitly copy raw buffer so Message can own the memory.
            m.raw.extend_from_slice(buf);

            if let Err(err) = m.decode() {
                log::warn!(
                    "Failed to handle decode ICE from {} to {}: {}",
                    addr,
                    src_addr,
                    err
                );
            } else {
                let agent_internal_clone = Arc::clone(agent_internal);
                let mut ai = agent_internal.lock().await;
                ai.handle_inbound(&mut m, c, src_addr, agent_internal_clone)
                    .await;
            }
        } else {
            let ai = agent_internal.lock().await;
            if !ai.validate_non_stun_traffic(c, src_addr).await {
                log::warn!(
                    "Discarded message from {}, not a valid remote candidate",
                    c.addr().await
                );
            } else if let Err(err) = ai.agent_conn.buffer.write(buf).await {
                // NOTE This will return packetio.ErrFull if the buffer ever manages to fill up.
                log::warn!("failed to write packet: {}", err);
            }
        }
    }
}
