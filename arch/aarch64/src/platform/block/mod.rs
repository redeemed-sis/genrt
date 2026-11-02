//! Concrete block-controller drivers. Firmware matching belongs to bootstrap;
//! each driver owns its controller protocol, DMA storage and request lifetime.

pub(crate) mod virtio_block;
