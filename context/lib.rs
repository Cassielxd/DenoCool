use state::Container;

pub static APPLICATION_CONTEXT: Container![Send + Sync] = <Container![Send + Sync]>::new();
