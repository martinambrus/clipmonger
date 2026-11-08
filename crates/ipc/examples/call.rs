//! Debug helper: call one daemon method and print the JSON result.
//!
//!   cargo run -p clipmonger-ipc --example call -- PROFILE METHOD '{"json":"params"}'
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [profile, method, params] = args.as_slice() else {
        eprintln!("usage: call PROFILE METHOD JSON");
        std::process::exit(2);
    };
    let profile = std::path::Path::new(profile);
    let rendezvous: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(profile.join("rendezvous.json")).unwrap())
            .unwrap();
    let token = std::fs::read_to_string(profile.join("token")).unwrap();
    let socket = rendezvous["socket"].as_str().unwrap();
    let client =
        clipmonger_ipc::client::Client::connect(std::path::Path::new(socket), token.trim()).unwrap();
    let started = std::time::Instant::now();
    let result = client.call(method, serde_json::from_str(params).unwrap());
    println!("{:?} after {:?}", result, started.elapsed());
}
