//! The motor engine: the gates' stand-in judge (W.E.1's chair with a
//! simple occupant). It logs every Event it receives and answers
//! each Parked with a Release when the destination is on its
//! allowlist, a Refusal when it is not. It is a gate fixture, not
//! a product: the real engine lives outside this repository.

use crate::pb;
use tokio_stream::StreamExt;

struct Motor {
    allow: Vec<(Vec<u8>, u32)>,
    /// Standing grants, emitted once when a bridge's stream opens:
    /// (ip-or-empty-for-arp, port, proto, keep_open seconds).
    /// Symmetric by the membrane's law -- one endpoint grant covers
    /// its flows in both directions, replies included. The proto
    /// matters: matching is exact, and 53/udp is not 53/tcp.
    grant: Vec<(Vec<u8>, u32, u32, u64)>,
    /// The example's memory rule: (ip-or-empty-for-arp, port,
    /// keep_open seconds). A released park matching one also gets
    /// a membrane_memory Decision -- the whole seam, demonstrated.
    remember: Vec<(Vec<u8>, u32, u64)>,
}

#[tonic::async_trait]
impl pb::engine_server::Engine for Motor {
    type DecideStream = tokio_stream::wrappers::ReceiverStream<Result<pb::Decision, tonic::Status>>;

    async fn decide(
        &self,
        req: tonic::Request<tonic::Streaming<pb::Event>>,
    ) -> Result<tonic::Response<Self::DecideStream>, tonic::Status> {
        let mut events = req.into_inner();
        let allow = self.allow.clone();
        let remember = self.remember.clone();
        let grant = self.grant.clone();
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        tokio::spawn(async move {
            // The standing grants land before the first verdict: a
            // memory rides the Decision oneof like any other word.
            for (ip, port, proto, keep_open) in &grant {
                let destination = if ip.is_empty() {
                    pb::Destination {
                        host: String::new(),
                        ip: Vec::new(),
                        port: 0,
                        proto: 0,
                        ethertype: 0x0806,
                        mac: Vec::new(),
                    }
                } else {
                    pb::Destination {
                        host: String::new(),
                        ip: ip.clone(),
                        port: *port,
                        proto: *proto,
                        ethertype: 0x0800,
                        mac: Vec::new(),
                    }
                };
                let d = pb::Decision {
                    id: Vec::new(),
                    decision: Some(pb::decision::Decision::MembraneMemory(pb::MembraneMemory {
                        destination: Some(destination),
                        skip_freeze: true,
                        keep_open: *keep_open,
                        written: 0,
                    })),
                };
                cella_libs::logln!("motor: grant standing (keep_open={keep_open}s)");
                if tx.send(Ok(d)).await.is_err() {
                    return;
                }
            }
            while let Some(next) = events.next().await {
                let ev = match next {
                    Ok(ev) => ev,
                    Err(e) => {
                        // A stream error is worth a line, not a
                        // silent end: the bridge retries frames,
                        // the motor keeps listening.
                        cella_libs::logln!("motor: stream error: {e}");
                        continue;
                    }
                };
                let Some(pb::event::Event::Parked(op)) = ev.event else {
                    // Completions and looks are evidence, not questions.
                    cella_libs::logln!("motor: event (not a park)");
                    continue;
                };
                let (ip, port, ethertype, dir) = match &op.destination {
                    Some(d) => (d.ip.clone(), d.port, d.ethertype, op.direction),
                    None => (Vec::new(), 0, 0, op.direction),
                };
                cella_libs::logln!(
                    "motor: parked id={} ip={} port={} dir={}",
                    cella_hex(&op.id),
                    ip.iter()
                        .map(|b| b.to_string())
                        .collect::<Vec<_>>()
                        .join("."),
                    port,
                    dir
                );
                // ARP alone rides free: policy speaks IPv4, and a
                // judge that refuses ARP darkens every destination,
                // allowed ones included. The exception is exactly
                // ethertype 0x0806 -- on the world plane a released
                // ARP reaches only the machine's own translator,
                // which answers it at the edge. Every other
                // non-IPv4 ethertype is refused like anything else
                // the policy cannot name.
                let arp = ethertype == 0x0806;
                let allowed = arp
                    || allow.iter().any(|(a_ip, a_port)| {
                        (a_ip.is_empty() || *a_ip == ip) && (*a_port == 0 || *a_port == port)
                    });
                let decision = if allowed {
                    pb::decision::Decision::Release(pb::Release {})
                } else {
                    pb::decision::Decision::Refusal(pb::Refusal {
                        why: "off the allowlist".into(),
                    })
                };
                let d = pb::Decision {
                    id: op.id.clone(),
                    decision: Some(decision),
                };
                cella_libs::logln!(
                    "motor: {} id={}",
                    if allowed { "release" } else { "refuse" },
                    cella_hex(&op.id)
                );
                if tx.send(Ok(d)).await.is_err() {
                    return;
                }
                // The memory rule, demonstrated: a park the rule
                // names also plants a standing memory -- the park's
                // own exact destination, skip_freeze, a window. The
                // bridge stamps written at the landing. The rule
                // fires on refusals too: a standing refusal with
                // skip_freeze is the instant-error row, no
                // freeze-thaw churn per denied attempt.
                {
                    let matched = remember.iter().find(|(r_ip, r_port, _)| {
                        match (r_ip.is_empty(), *r_port) {
                            // arp:secs -- the L2 rule.
                            (true, 0) => arp && ethertype == 0x0806,
                            // *:port -- any released park on this
                            // port plants its own exact memory (the
                            // named-world case: the destination is
                            // unknowable at policy time).
                            (true, p) => !ip.is_empty() && p == port,
                            (false, _) => *r_ip == ip && *r_port == port,
                        }
                    });
                    if let Some((_, _, keep_open)) = matched {
                        let mem = pb::Decision {
                            id: Vec::new(),
                            decision: Some(pb::decision::Decision::MembraneMemory(
                                pb::MembraneMemory {
                                    destination: op.destination.clone(),
                                    skip_freeze: true,
                                    keep_open: *keep_open,
                                    written: 0,
                                },
                            )),
                        };
                        cella_libs::logln!("motor: remember keep_open={keep_open}s");
                        if tx.send(Ok(mem)).await.is_err() {
                            return;
                        }
                    }
                }
            }
        });
        Ok(tonic::Response::new(
            tokio_stream::wrappers::ReceiverStream::new(rx),
        ))
    }
}

fn cella_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Parse "a.b.c.d:port" into the allow shape; "*" fields match all.
fn parse_allow(s: &str) -> Result<(Vec<u8>, u32), String> {
    let (ip, port) = s
        .rsplit_once(':')
        .ok_or_else(|| format!("allow {s:?}: want ip:port"))?;
    let ip_bytes = if ip == "*" {
        Vec::new()
    } else {
        let parts: Vec<u8> = ip
            .split('.')
            .map(|p| p.parse::<u8>())
            .collect::<Result<_, _>>()
            .map_err(|e| format!("allow {s:?}: {e}"))?;
        if parts.len() != 4 {
            return Err(format!("allow {s:?}: want four octets"));
        }
        parts
    };
    let port = if port == "*" {
        0
    } else {
        port.parse::<u32>()
            .map_err(|e| format!("allow {s:?}: {e}"))?
    };
    Ok((ip_bytes, port))
}

pub fn run(args: &[String]) -> Result<(), String> {
    let mut listen = None;
    let mut allow = Vec::new();
    let mut remember = Vec::new();
    let mut grant = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--listen" => listen = it.next().cloned(),
            "--allow" => {
                let v = it.next().ok_or("--allow needs ip:port")?;
                allow.push(parse_allow(v)?);
            }
            "--grant" => {
                // ip:port/proto:keep_open_s or arp:keep_open_s -- a
                // standing symmetric endpoint grant, sent at
                // stream-open. Exact means exact: the proto rides.
                let v = it
                    .next()
                    .ok_or("--grant needs ip:port/proto:secs or arp:secs")?;
                let (head, secs) = v
                    .rsplit_once(':')
                    .ok_or_else(|| format!("grant {v:?}: want ip:port/proto:secs or arp:secs"))?;
                let keep_open: u64 = secs.parse().map_err(|e| format!("grant {v:?}: {e}"))?;
                if head == "arp" {
                    grant.push((Vec::new(), 0, 0, keep_open));
                } else {
                    let (addr, protoword) = head
                        .rsplit_once('/')
                        .ok_or_else(|| format!("grant {v:?}: no /proto"))?;
                    let proto: u32 = match protoword {
                        "tcp" => 6,
                        "udp" => 17,
                        n => n.parse().map_err(|e| format!("grant {v:?}: {e}"))?,
                    };
                    let (ip, port) = parse_allow(addr)?;
                    grant.push((ip, port, proto, keep_open));
                }
            }
            "--remember" => {
                // ip:port:keep_open_s, arp:keep_open_s, or
                // *:port:keep_open_s (any released park on the
                // port plants its own exact memory) -- the
                // example's memory rule shapes.
                let v = it
                    .next()
                    .ok_or("--remember needs ip:port:secs or arp:secs")?;
                let (head, secs) = v
                    .rsplit_once(':')
                    .ok_or_else(|| format!("remember {v:?}: want ip:port:secs or arp:secs"))?;
                let keep_open: u64 = secs.parse().map_err(|e| format!("remember {v:?}: {e}"))?;
                if head == "arp" {
                    remember.push((Vec::new(), 0, keep_open));
                } else {
                    let (ip, port) = parse_allow(head)?;
                    remember.push((ip, port, keep_open));
                }
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    let listen = listen.ok_or("--listen is required")?;
    let addr = listen
        .parse()
        .map_err(|e| format!("listen {listen:?}: {e}"))?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async move {
        cella_libs::logln!("motor: listening on {listen}");
        tonic::transport::Server::builder()
            .add_service(pb::engine_server::EngineServer::new(Motor {
                allow,
                remember,
                grant,
            }))
            .serve(addr)
            .await
            .map_err(|e| e.to_string())
    })
}
