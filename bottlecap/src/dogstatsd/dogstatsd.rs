pub struct DogStatsD<'a> {
    host: &'a str,
    port: u16,
}

impl<'a> DogStatsD<'a> {
    pub fn new(host: &'a str, port: u16) -> Self {
        DogStatsD { host, port }
    }

    // todo: better error handling
    pub fn run(&self) {
        let addr = format!("{}:{}", self.host, self.port);
        let _ = std::thread::spawn(move || {
            let socket = std::net::UdpSocket::bind(addr).expect("couldn't bind to address");
            loop {
                let mut buf = [0; 1024]; // todo, do we want to make this dynamic? (not sure)
                let (amt, src) = socket.recv_from(&mut buf).expect("didn't receive data");
                let buf = &mut buf[..amt];
                let msg = std::str::from_utf8(buf).expect("couldn't parse as string");
                log::info!("received message: {} from {}", msg, src);
            }
        });
    }
}
