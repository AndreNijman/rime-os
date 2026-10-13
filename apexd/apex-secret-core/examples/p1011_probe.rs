//! SCRATCH — a live probe for unit p1-011. DELETE BEFORE COMMITTING.
//!
//! Sends one `Request::Use` to a private `apex-secretd`, the way
//! `apex-secretd/tests/end_to_end.rs` does, because `apex secret use` goes
//! through `apex-agentd` and the live one on this machine is off limits.
//!
//! argv: <socket> <project> <service> <operation> [resource] [name=value ...]

use apex_secret_core::capability::CapabilityRecord;
use apex_secret_core::client::Client;
use apex_secret_core::protocol::Request;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 4 {
        eprintln!("usage: p1011_probe <socket> <project> <service> <operation> [resource] [k=v ..]");
        std::process::exit(2);
    }
    let socket = std::path::PathBuf::from(&args[0]);
    let project = args[1].clone();
    let service = &args[2];
    let operation = &args[3];
    let resource = args.get(4).cloned().unwrap_or_default();

    let mut rec = CapabilityRecord::new(service, operation, &resource);
    rec.project = Some(project);
    for pair in args.iter().skip(5) {
        let (k, v) = pair.split_once('=').expect("k=v");
        rec.params.insert(k.to_string(), v.to_string());
    }

    let mut client = Client::connect_at(&socket).expect("connect");
    let reply = client
        .request(&Request::Use {
            body_len: 0,
            record: Box::new(rec),
        })
        .expect("use");
    println!("{}", serde_json::to_string_pretty(&reply).expect("json"));
}
