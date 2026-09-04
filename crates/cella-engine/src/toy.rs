//! The toy engine: the gates' stand-in judge (W.E.1's chair with a
//! simple occupant). It logs every Event it receives and answers
//! each Parked with a Release when the destination is on its
//! allowlist, a Refusal when it is not. It is a gate fixture, not
//! a product: the real engine lives outside this repository.

use crate::pb;
use tokio_stream::StreamExt;

struct Toy {
    allow: Vec<(Vec<u8>, u32)>,
}

#[tonic::async_trait]
impl pb::engine_server::Engine for Toy {
    type DecideStream = tokio_stream::wrappers::ReceiverStream<Result<pb::Decision, tonic::Status>>;

    async fn decide(
        &self,
        req: tonic::Request<tonic::Streaming<pb::Event>>,
    ) -> Result<tonic::Response<Self::DecideStream>, tonic::Status> {
        let mut events = req.into_inner();
        let allow = self.allow.clone();
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        tokio::spawn(async move {
            while let Some(Ok(ev)) = events.next().await {
                let Some(pb::event::Event::Parked(op)) = ev.event else {
                    // Completions and looks are evidence, not questions.
                    println!("toy: event (not a park)");
                    continue;
                };
                let (ip, port, dir) = match &op.destination {
                    Some(d) => (d.ip.clone(), d.port, op.direction),
                    None => (Vec::new(), 0, op.direction),
                };
                println!(
                    "toy: parked id={} ip={} port={} dir={}",
                    cella_hex(&op.id),
                    ip.iter()
                        .map(|b| b.to_string())
                        .collect::<Vec<_>>()
                        .join("."),
                    port,
                    dir
                );
                // An L2 park (ARP, or anything the membrane named
                // by ethertype and MAC alone) carries no IPv4
                // triple. Policy speaks IPv4; the wire's grammar
                // releases: refusing ARP would darken every
                // destination, allowed ones included.
                let l2 = ip.is_empty() && port == 0;
                let allowed = l2
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
                println!(
                    "toy: {} id={}",
                    if allowed { "release" } else { "refuse" },
                    cella_hex(&op.id)
                );
                if tx.send(Ok(d)).await.is_err() {
                    return;
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
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--listen" => listen = it.next().cloned(),
            "--allow" => {
                let v = it.next().ok_or("--allow needs ip:port")?;
                allow.push(parse_allow(v)?);
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
        println!("toy: listening on {listen}");
        tonic::transport::Server::builder()
            .add_service(pb::engine_server::EngineServer::new(Toy { allow }))
            .serve(addr)
            .await
            .map_err(|e| e.to_string())
    })
}
