use superliquid::{
    config, console,
    node::{runner::run_node, state::PeerInfo},
};

use std::env;

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();
    let ip = "127.0.0.1";
    let base_port = 6400;
    let client_port = 8000;
    let num_nodes = config::retrieve_num_validators();

    let node_index = args
        .get(2)
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0);

    if node_index >= num_nodes {
        panic!("Invalid node index. Must be 0 to {}", num_nodes - 1);
    }

    let ports: Vec<u16> = (0..num_nodes).map(|i| base_port + (i as u16)).collect();
    let client_ports: Vec<u16> = (0..num_nodes).map(|i| client_port + (i as u16)).collect();
    let curr_port = ports[node_index];
    let cli_port = client_ports[node_index];
    let consensus_addr = format!("{}:{}", ip, curr_port);
    let client_addr = format!("{}:{}", ip, cli_port);

    let peers = ports
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != node_index)
        .map(|(i, port)| (i, format!("{}:{}", ip, port)))
        .map(|(i, addr)| PeerInfo {
            peer_id: i,
            peer_addr: addr,
        })
        .collect();

    let _ = match args.get(1).map(|s| s.as_str()) {
        Some("node") => run_node(client_addr, consensus_addr, peers, node_index).await,
        Some("console") => console::run_console(&client_addr).await,
        _ => {
            eprintln!("Usage: cargo run -- [node|console] [number]");
            Ok(())
        }
    };
}
