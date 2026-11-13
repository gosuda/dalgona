pub fn start() {}

pub struct Config {
    pub name: String,
}

pub enum Mode {
    Fast,
}

pub trait Runner {
    fn run(&self);
}

impl Config {
    pub fn new() -> Config {
        Config { name: String::new() }
    }
}
