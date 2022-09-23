use http_utils::{http_request, http_response};
use sozu_command_lib::{
    config::{Config, FileConfig},
    proxy::{ActivateListener, ListenerType, ProxyRequestOrder},
    scm_socket::Listeners,
};
use std::{io::stdin, net::SocketAddr};

mod http_utils;
mod mock;
mod sozu;

use sozu::worker::Worker;

use mock::{
    aggregator::SimpleAggregator, async_backend::Backend as AsyncBackend, client::Client,
    sync_backend::Backend as SyncBackend,
};

const BUFFER_SIZE: usize = 4096;

/// Setup a Sozu worker with
/// - `config`
/// - `listeners`
/// - 1 active HttpListener on `front_address`
/// - 1 cluster ("cluster_0")
/// - 1 HttpFrontend for "cluster_0" on `front_address`
/// - n backends ("cluster_0-{0..n}")
fn test_setup(
    config: Config,
    listeners: Listeners,
    front_address: SocketAddr,
    nb_backends: usize,
) -> (Worker, Vec<SocketAddr>) {
    let mut worker = Worker::start_new_worker(config, listeners);

    worker.send_proxy_request(ProxyRequestOrder::AddHttpListener(
        Worker::default_http_listener(front_address),
    ));
    worker.send_proxy_request(ProxyRequestOrder::ActivateListener(ActivateListener {
        address: front_address,
        proxy: ListenerType::HTTP,
        from_scm: false,
    }));
    worker.send_proxy_request(ProxyRequestOrder::AddCluster(Worker::default_cluster(
        "cluster_0",
    )));
    worker.send_proxy_request(ProxyRequestOrder::AddHttpFrontend(
        Worker::default_http_frontend("cluster_0", front_address),
    ));

    let mut backends = Vec::new();
    for i in 0..nb_backends {
        let back_address = format!("127.0.0.1:{}", 2002 + i)
            .parse()
            .expect("could not parse back address");
        worker.send_proxy_request(ProxyRequestOrder::AddBackend(Worker::default_backend(
            "cluster_0",
            format!("cluster_0-{}", i),
            back_address,
        )));
        backends.push(back_address);
    }

    worker.read_to_last();
    (worker, backends)
}

fn async_test_setup(
    config: Config,
    listeners: Listeners,
    front_address: SocketAddr,
    nb_backends: usize,
) -> (Worker, Vec<AsyncBackend<SimpleAggregator>>) {
    let (worker, backends) = test_setup(config, listeners, front_address, nb_backends);
    let backends = backends
        .into_iter()
        .enumerate()
        .map(|(i, back_address)| {
            let aggregator = SimpleAggregator {
                received: 0,
                sent: 0,
            };
            AsyncBackend::new(
                format!("BACKEND_{}", i),
                back_address,
                aggregator.to_owned(),
                AsyncBackend::http_handler(format!("pong{}", i)),
            )
        })
        .collect::<Vec<_>>();
    (worker, backends)
}

fn sync_test_setup(
    config: Config,
    listeners: Listeners,
    front_address: SocketAddr,
    nb_backends: usize,
) -> (Worker, Vec<SyncBackend>) {
    let (worker, backends) = test_setup(config, listeners, front_address, nb_backends);
    let backends = backends
        .into_iter()
        .enumerate()
        .map(|(i, back_address)| {
            SyncBackend::new(
                format!("BACKEND_{}", i),
                back_address,
                http_response(format!("pong{}", i)),
            )
        })
        .collect::<Vec<_>>();
    (worker, backends)
}

fn test_async(nb_backends: usize, nb_clients: usize, nb_requests: usize) {
    let front_address = "127.0.0.1:2001"
        .parse()
        .expect("could not parse front address");

    let (config, listeners) = Worker::empty_config();
    let (mut worker, mut backends) =
        async_test_setup(config, listeners, front_address, nb_backends);

    let mut clients = (0..nb_clients)
        .map(|i| {
            Client::new(
                format!("client{}", i),
                front_address,
                http_request("GET", "/api", format!("ping{}", i)),
            )
        })
        .collect::<Vec<_>>();
    for client in clients.iter_mut() {
        client.connect();
    }
    for _ in 0..nb_requests {
        for client in clients.iter_mut() {
            client.send();
        }
        for client in clients.iter_mut() {
            match client.receive() {
                Some(response) => println!("{}", response),
                _ => {}
            }
        }
    }

    worker.send_proxy_request(ProxyRequestOrder::HardStop);
    worker.wait();

    for client in clients {
        println!(
            "{} sent: {}, received: {}",
            client.name, client.sent, client.received
        );
    }
    for backend in backends.iter_mut() {
        let aggregator = backend.stop_and_get_aggregator();
        println!("{} aggregated: {:?}", backend.name, aggregator);
    }
}

fn test_sync(nb_clients: usize, nb_requests: usize) {
    let front_address = "127.0.0.1:2001"
        .parse()
        .expect("could not parse front address");

    let (config, listeners) = Worker::empty_config();
    let (mut worker, mut backends) = sync_test_setup(config, listeners, front_address, 1);
    let mut backend = backends.pop().unwrap();

    backend.connect();

    let mut clients = (0..nb_clients)
        .map(|i| {
            Client::new(
                format!("client{}", i),
                front_address,
                http_request("GET", "/api", format!("ping{}", i)),
            )
        })
        .collect::<Vec<_>>();
    for (i, client) in clients.iter_mut().enumerate() {
        client.connect();
        client.send();
        backend.accept(i);
        backend.receive(i);
        backend.send(i);
        client.receive();
    }
    for _ in 0..nb_requests {
        for client in clients.iter_mut() {
            client.send();
        }
        for i in 0..nb_clients {
            backend.receive(i);
            backend.send(i);
        }
        for client in clients.iter_mut() {
            match client.receive() {
                Some(response) => println!("{}", response),
                _ => {}
            }
        }
    }

    worker.send_proxy_request(ProxyRequestOrder::HardStop);
    worker.wait();

    for client in clients {
        println!(
            "{} sent: {}, received: {}",
            client.name, client.sent, client.received
        );
    }
    println!(
        "{} sent: {}, received: {}",
        backend.name, backend.sent, backend.received
    );
}

fn test_backend_stop(nb_requests: usize, zombie: Option<u32>) {
    let front_address = "127.0.0.1:2001"
        .parse()
        .expect("could not parse front address");

    let config = Worker::into_config(FileConfig {
        zombie_check_interval: zombie,
        ..Worker::empty_file_config()
    });
    let listeners = Worker::empty_listeners();
    let (mut worker, mut backends) = async_test_setup(config, listeners, front_address, 2);
    let mut backend2 = backends.pop().expect("backend2");
    let mut backend1 = backends.pop().expect("backend1");

    let mut aggregator = Some(SimpleAggregator {
        received: 0,
        sent: 0,
    });

    let mut client = Client::new("client", front_address, http_request("GET", "/api", "ping"));
    client.connect();

    for i in 0..nb_requests {
        if client.send().is_none() {
            break;
        }
        match client.receive() {
            Some(response) => println!("{}", response),
            None => break,
        }
        if i == 0 {
            aggregator = backend1.stop_and_get_aggregator();
        }
    }

    worker.send_proxy_request(ProxyRequestOrder::HardStop);
    worker.wait();

    println!("sent: {}, received: {}", client.sent, client.received);
    println!("backend1 aggregator: {:?}", aggregator);
    aggregator = backend2.stop_and_get_aggregator();
    println!("backend2 aggregator: {:?}", aggregator);
}

fn test_issue_806() {
    test_backend_stop(2, None);
}
fn test_issue_808() {
    test_backend_stop(2, Some(1));
}

fn test_issue_810_timeout() {
    let front_address = "127.0.0.1:2001"
        .parse()
        .expect("could not parse front address");

    let (config, listeners) = Worker::empty_config();
    let (mut worker, mut backends) = sync_test_setup(config, listeners, front_address, 1);
    let mut backend = backends.pop().unwrap();

    let mut client = Client::new("client", front_address, http_request("GET", "/api", "ping"));

    backend.connect();
    client.connect();
    client.send();
    backend.accept(0);
    backend.receive(0);
    backend.send(0);
    client.receive();

    worker.send_proxy_request(ProxyRequestOrder::SoftStop);
    worker.wait();

    println!(
        "{} sent: {}, received: {}",
        client.name, client.sent, client.received
    );
    println!(
        "{} sent: {}, received: {}",
        backend.name, backend.sent, backend.received
    );
}

fn test_issue_810_panic(part2: bool) {
    let front_address = "127.0.0.1:2001"
        .parse()
        .expect("could not parse front address");
    let back_address = format!("127.0.0.1:2002")
        .parse()
        .expect("could not parse back address");

    let (config, listeners) = Worker::empty_config();
    let mut worker = Worker::start_new_worker(config, listeners);

    worker.send_proxy_request(ProxyRequestOrder::AddTcpListener(
        Worker::default_tcp_listener(front_address),
    ));
    worker.send_proxy_request(ProxyRequestOrder::ActivateListener(ActivateListener {
        address: front_address,
        proxy: ListenerType::TCP,
        from_scm: false,
    }));
    worker.send_proxy_request(ProxyRequestOrder::AddCluster(Worker::default_cluster(
        "cluster_0",
    )));
    worker.send_proxy_request(ProxyRequestOrder::AddTcpFrontend(
        Worker::default_tcp_frontend("cluster_0", front_address),
    ));

    worker.send_proxy_request(ProxyRequestOrder::AddBackend(Worker::default_backend(
        "cluster_0",
        "cluster_0-0",
        back_address,
    )));
    worker.read_to_last();

    let mut backend = SyncBackend::new("backend", back_address, "pong");
    let mut client = Client::new("client", front_address, "ping");

    backend.connect();
    client.connect();
    client.send();
    if !part2 {
        backend.accept(0);
        backend.receive(0);
        backend.send(0);
        client.receive();
    }

    worker.send_proxy_request(ProxyRequestOrder::SoftStop);
    worker.wait();

    println!(
        "{} sent: {}, received: {}",
        client.name, client.sent, client.received
    );
    println!(
        "{} sent: {}, received: {}",
        backend.name, backend.sent, backend.received
    );
}

fn wait_input<S: Into<String>>(s: S) {
    println!("==================================================================");
    println!("{}", s.into());
    println!("==================================================================");
    let mut buf = String::new();
    stdin().read_line(&mut buf).expect("bad input");
}

fn main() {
    wait_input("test_sync");
    test_sync(10, 100);
    wait_input("test_async");
    test_async(3, 10, 100);
    // https://github.com/sozu-proxy/sozu/issues/806
    wait_input("issue 806: timeout with invalid back token");
    test_issue_806();
    // https://github.com/sozu-proxy/sozu/issues/808
    wait_input("issue 808: panic on successful zombie check");
    test_issue_808();
    // https://github.com/sozu-proxy/sozu/issues/810
    wait_input("issue 810: shutdown struggles until session timeout");
    test_issue_810_timeout();
    wait_input("issue 810: shutdown panics on session close");
    test_issue_810_panic(false);
    wait_input("issue 810: shutdown panics on tcp connection after proxy cleared its listeners");
    test_issue_810_panic(true);
}
