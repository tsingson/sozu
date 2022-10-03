use std::{
    collections::HashMap,
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    str::from_utf8,
};

use crate::BUFFER_SIZE;

/// A mock backend whose actions are all synchronous (accepting, receiving, responding...)
/// this should help reproductibility by enforcing a strict order on those actions
pub struct Backend {
    pub name: String,
    pub address: SocketAddr,
    pub listener: Option<TcpListener>,
    pub clients: HashMap<usize, TcpStream>,
    pub response: String,
    pub sent: usize,
    pub received: usize,
}

impl Backend {
    pub fn new<S1: Into<String>, S2: Into<String>>(
        name: S1,
        address: SocketAddr,
        response: S2,
    ) -> Self {
        let name = name.into();
        let response = response.into();
        Self {
            name,
            address,
            listener: None,
            clients: HashMap::new(),
            response,
            sent: 0,
            received: 0,
        }
    }
    pub fn connect(&mut self) {
        let listener = TcpListener::bind(self.address).expect("could not bind");
        self.listener = Some(listener);
        self.clients = HashMap::new();
    }
    pub fn disconnect(&mut self) {
        self.listener = None;
        self.clients = HashMap::new();
    }
    pub fn accept(&mut self, client_id: usize) -> bool {
        if let Some(listener) = &self.listener {
            let stream = listener.accept();
            match stream {
                Ok(stream) => {
                    self.clients.insert(client_id, stream.0);
                    return true;
                }
                Err(error) => {
                    println!("accept error: {:?}", error);
                }
            }
        }
        false
    }
    pub fn send(&mut self, client_id: usize) -> Option<usize> {
        match self.clients.get_mut(&client_id) {
            Some(stream) => match stream.write(self.response.as_bytes()) {
                Ok(0) => {
                    println!("{} received nothing", self.name);
                }
                Ok(n) => {
                    println!("{} sent {} to {}", self.name, n, client_id);
                    self.sent += 1;
                    return Some(n);
                }
                Err(error) => {
                    println!(
                        "{} could not respond to {}: {}",
                        self.name, client_id, error
                    );
                }
            },
            None => {
                println!("no client with id {} on backend {}", client_id, self.name);
            }
        }
        None
    }
    pub fn receive(&mut self, client_id: usize) -> Option<String> {
        match self.clients.get_mut(&client_id) {
            Some(stream) => {
                let mut buf = [0u8; BUFFER_SIZE];
                match stream.read(&mut buf) {
                    Ok(0) => {
                        println!("{} sent nothing", self.name);
                    }
                    Ok(n) => {
                        println!("{} received {} from {}", self.name, n, client_id);
                        self.received += 1;
                        return Some(from_utf8(&buf[..n]).unwrap().to_string());
                    }
                    Err(error) => {
                        println!(
                            "{} could not receive for {}: {}",
                            self.name, client_id, error
                        );
                    }
                }
            }
            None => {
                println!("no client with id {} on backend {}", client_id, self.name);
            }
        }
        None
    }
}
