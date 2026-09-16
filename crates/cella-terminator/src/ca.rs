//! The pair CA's working end: load the baked key, mint one leaf
//! per name, cache the minted (tasks/PHASE2-security.md, 2.7 (c)).
//! The key never leaves the image; this module never serializes it
//! anywhere but memory.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;

use rustls::pki_types::{CertificateDer, PrivateKeyDer};

pub struct Minter {
    ca_cert: rcgen::Certificate,
    ca_key: rcgen::KeyPair,
    /// The baked CA cert's DER, sent in every chain so a member can
    /// verify against its trust store without fetching anything.
    ca_der: CertificateDer<'static>,
    minted: Mutex<HashMap<String, Arc<rustls::server::ServerConfig>>>,
}

impl Minter {
    /// Load the baked pair CA. The cert PEM is re-parsed into
    /// signing params with its subject preserved, so a minted
    /// leaf's issuer chains to the baked trust anchor.
    pub fn load(cert_pem_path: &Path, key_pem_path: &Path) -> Result<Minter, String> {
        let cert_pem = std::fs::read_to_string(cert_pem_path)
            .map_err(|e| format!("reading {}: {e}", cert_pem_path.display()))?;
        let key_pem = std::fs::read_to_string(key_pem_path)
            .map_err(|e| format!("reading {}: {e}", key_pem_path.display()))?;
        let ca_key = rcgen::KeyPair::from_pem(&key_pem).map_err(|e| format!("CA key: {e}"))?;
        let ca_der = pem_to_der(&cert_pem)?;
        let params = rcgen::CertificateParams::from_ca_cert_der(&ca_der)
            .map_err(|e| format!("CA cert: {e}"))?;
        let ca_cert = params
            .self_signed(&ca_key)
            .map_err(|e| format!("CA rebuild: {e}"))?;
        Ok(Minter {
            ca_cert,
            ca_key,
            ca_der,
            minted: Mutex::new(HashMap::new()),
        })
    }

    /// The server config for one SNI: minted once, cached forever
    /// (the appliance's lifetime is the machine's).
    pub fn server_config_for(
        &self,
        name: &str,
    ) -> Result<Arc<rustls::server::ServerConfig>, String> {
        if let Some(c) = self.minted.lock().unwrap().get(name) {
            return Ok(c.clone());
        }
        let leaf_key = rcgen::KeyPair::generate().map_err(|e| e.to_string())?;
        let mut params =
            rcgen::CertificateParams::new(vec![name.to_string()]).map_err(|e| e.to_string())?;
        params.distinguished_name = rcgen::DistinguishedName::new();
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, name);
        // The frozen-member corollary (2.7 (c)): a member that slept
        // a year validates this leaf against its own past clock. The
        // window is the pair's lifetime, not the world's calendar.
        params.not_before = rcgen::date_time_ymd(1975, 1, 1);
        params.not_after = rcgen::date_time_ymd(2200, 1, 1);
        // The member's verifier is not ours to choose, so the leaf
        // serves the strictest honest one (RFC 5280): the AKI names
        // the issuing pair CA, and the usages say exactly what a
        // TLS server leaf is for -- nothing implicit, nothing more.
        params.use_authority_key_identifier_extension = true;
        params.key_usages = vec![rcgen::KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
        let leaf = params
            .signed_by(&leaf_key, &self.ca_cert, &self.ca_key)
            .map_err(|e| format!("minting {name}: {e}"))?;
        let chain = vec![
            CertificateDer::from(leaf.der().to_vec()),
            self.ca_der.clone(),
        ];
        let key = PrivateKeyDer::try_from(leaf_key.serialize_der()).map_err(|e| e.to_string())?;
        let cfg = rustls::server::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(chain, key)
            .map_err(|e| e.to_string())?;
        let cfg = Arc::new(cfg);
        self.minted
            .lock()
            .unwrap()
            .insert(name.to_string(), cfg.clone());
        Ok(cfg)
    }
}

fn pem_to_der(pem: &str) -> Result<CertificateDer<'static>, String> {
    let mut reader = std::io::BufReader::new(pem.as_bytes());
    let cert = rustls_pemfile::certs(&mut reader)
        .next()
        .ok_or("no certificate in the CA pem")?
        .map_err(|e| e.to_string())?;
    Ok(cert)
}

/// Mint a fresh pair CA -- the image *build*'s door, used by
/// `cella build rootfs terminator` (phase C) and by the tests.
/// Returns (cert_pem, key_pem).
pub fn mint_pair_ca(pair_name: &str) -> Result<(String, String), String> {
    let key = rcgen::KeyPair::generate().map_err(|e| e.to_string())?;
    let mut params =
        rcgen::CertificateParams::new(Vec::<String>::new()).map_err(|e| e.to_string())?;
    params.distinguished_name = rcgen::DistinguishedName::new();
    params.distinguished_name.push(
        rcgen::DnType::CommonName,
        format!("cella pair CA {pair_name}"),
    );
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Constrained(0));
    params.not_before = rcgen::date_time_ymd(1975, 1, 1);
    params.not_after = rcgen::date_time_ymd(2200, 1, 1);
    params.key_usages = vec![
        rcgen::KeyUsagePurpose::KeyCertSign,
        rcgen::KeyUsagePurpose::CrlSign,
    ];
    let cert = params.self_signed(&key).map_err(|e| e.to_string())?;
    Ok((cert.pem(), key.serialize_pem()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("cella-term-ca-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        d
    }

    #[test]
    fn a_minted_leaf_satisfies_a_strict_verifier() {
        // rustls tolerates a bare leaf; a member's stricter stack
        // is entitled not to (RFC 5280). The leaf must carry the
        // AKI naming the pair CA and exactly the TLS-server
        // usages -- read back from the served bytes.
        use x509_parser::prelude::*;
        let (ca_pem, key_pem) = mint_pair_ca("strict").unwrap();
        let d = tmpdir();
        std::fs::write(d.join("strict-ca.pem"), &ca_pem).unwrap();
        std::fs::write(d.join("strict-ca.key"), &key_pem).unwrap();
        let minter = Minter::load(&d.join("strict-ca.pem"), &d.join("strict-ca.key")).unwrap();
        let leaf_der = handshake_peer_leaf(&minter, &ca_pem, "svc.example");
        let (_, leaf) = X509Certificate::from_der(&leaf_der).unwrap();
        let ca_der = super::pem_to_der(&ca_pem).unwrap();
        let (_, ca) = X509Certificate::from_der(ca_der.as_ref()).unwrap();
        // The AKI names the pair CA's own key identifier.
        let aki = leaf
            .get_extension_unique(&oid_registry::OID_X509_EXT_AUTHORITY_KEY_IDENTIFIER)
            .unwrap()
            .expect("the leaf carries no AKI");
        let ski = ca
            .get_extension_unique(&oid_registry::OID_X509_EXT_SUBJECT_KEY_IDENTIFIER)
            .unwrap()
            .expect("the CA carries no SKI");
        let ParsedExtension::AuthorityKeyIdentifier(aki) = aki.parsed_extension() else {
            panic!("AKI did not parse");
        };
        let ParsedExtension::SubjectKeyIdentifier(ski) = ski.parsed_extension() else {
            panic!("SKI did not parse");
        };
        assert_eq!(aki.key_identifier.as_ref().unwrap().0, ski.0);
        // Exactly the TLS-server usages, nothing implicit.
        let ku = leaf.key_usage().unwrap().expect("no KeyUsage");
        assert!(ku.value.digital_signature());
        assert!(!ku.value.key_cert_sign());
        let eku = leaf.extended_key_usage().unwrap().expect("no EKU");
        assert!(eku.value.server_auth);
        assert!(!eku.value.client_auth);
    }

    /// Handshake against the minter over loopback and return the
    /// leaf the server actually served -- the member's view.
    fn handshake_peer_leaf(minter: &Minter, ca_pem: &str, sni: &str) -> Vec<u8> {
        let cfg = minter.server_config_for(sni).unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots.add(pem_to_der(ca_pem).unwrap()).unwrap();
        let client_cfg = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut tcp, _) = listener.accept().unwrap();
            let mut conn = rustls::ServerConnection::new(cfg).unwrap();
            let mut tls = rustls::Stream::new(&mut conn, &mut tcp);
            use std::io::Read as _;
            let _ = tls.read(&mut [0u8; 1]);
        });
        let mut tcp = std::net::TcpStream::connect(addr).unwrap();
        let mut conn = rustls::ClientConnection::new(
            Arc::new(client_cfg),
            sni.to_string().try_into().unwrap(),
        )
        .unwrap();
        {
            let mut tls = rustls::Stream::new(&mut conn, &mut tcp);
            use std::io::Write as _;
            tls.write_all(b"x").unwrap();
        }
        let leaf = conn.peer_certificates().unwrap()[0].to_vec();
        drop(tcp);
        let _ = server.join();
        leaf
    }

    #[test]
    fn a_minted_leaf_chains_to_the_baked_anchor() {
        let (ca_pem, key_pem) = mint_pair_ca("test").unwrap();
        let d = tmpdir();
        std::fs::write(d.join("ca.pem"), &ca_pem).unwrap();
        std::fs::write(d.join("ca.key"), &key_pem).unwrap();
        let minter = Minter::load(&d.join("ca.pem"), &d.join("ca.key")).unwrap();
        let cfg = minter.server_config_for("svc.example").unwrap();

        // A member: rustls client trusting exactly the baked pem.
        let mut roots = rustls::RootCertStore::empty();
        roots.add(pem_to_der(&ca_pem).unwrap()).unwrap();
        let client_cfg = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();

        // Handshake over a local socket: the member dials the SNI,
        // the minted leaf must satisfy the baked trust store.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut tcp, _) = listener.accept().unwrap();
            let mut conn = rustls::ServerConnection::new(cfg).unwrap();
            let mut tls = rustls::Stream::new(&mut conn, &mut tcp);
            use std::io::{Read, Write};
            let mut buf = [0u8; 5];
            tls.read_exact(&mut buf).unwrap();
            tls.write_all(b"pong!").unwrap();
            buf
        });
        let mut tcp = std::net::TcpStream::connect(addr).unwrap();
        let name = rustls::pki_types::ServerName::try_from("svc.example").unwrap();
        let mut conn = rustls::ClientConnection::new(Arc::new(client_cfg), name).unwrap();
        let mut tls = rustls::Stream::new(&mut conn, &mut tcp);
        use std::io::{Read, Write};
        tls.write_all(b"ping!").unwrap();
        let mut buf = [0u8; 5];
        tls.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"pong!");
        assert_eq!(&server.join().unwrap(), b"ping!");

        // The cache: the second ask is the same Arc.
        let again = minter.server_config_for("svc.example").unwrap();
        assert_eq!(
            minter.minted.lock().unwrap().len(),
            1,
            "one name, one minting"
        );
        drop(again);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_wrong_name_is_refused_by_the_member() {
        let (ca_pem, key_pem) = mint_pair_ca("test2").unwrap();
        let d = tmpdir();
        std::fs::write(d.join("ca2.pem"), &ca_pem).unwrap();
        std::fs::write(d.join("ca2.key"), &key_pem).unwrap();
        let minter = Minter::load(&d.join("ca2.pem"), &d.join("ca2.key")).unwrap();
        let cfg = minter.server_config_for("right.example").unwrap();

        let mut roots = rustls::RootCertStore::empty();
        roots.add(pem_to_der(&ca_pem).unwrap()).unwrap();
        let client_cfg = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((mut tcp, _)) = listener.accept() {
                let mut conn = rustls::ServerConnection::new(cfg).unwrap();
                let mut tls = rustls::Stream::new(&mut conn, &mut tcp);
                use std::io::Read;
                let _ = tls.read(&mut [0u8; 1]);
            }
        });
        let mut tcp = std::net::TcpStream::connect(addr).unwrap();
        let name = rustls::pki_types::ServerName::try_from("wrong.example").unwrap();
        let mut conn = rustls::ClientConnection::new(Arc::new(client_cfg), name).unwrap();
        let mut tls = rustls::Stream::new(&mut conn, &mut tcp);
        use std::io::Write;
        let err = tls.write_all(b"x").and_then(|_| tls.flush());
        assert!(err.is_err(), "a leaf for the wrong name must not verify");
    }
}
