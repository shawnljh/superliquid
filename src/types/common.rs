// Helper struct to declare the capacity of queues
// The capacity of each queue should be a config defined value so that its tunable
// and should panic on the reciept of an invalid value
//
// QueueCapacity should hold a value that is a power of 2 for fast modulo
pub(crate) struct QueueCapacity(usize);

impl QueueCapacity {
    pub(crate) fn new(exponent: u32) -> Self {
        assert!(
            exponent < usize::BITS,
            "queue capacity exponent must be < {}, got {}",
            usize::BITS,
            exponent
        );
        Self(1usize << exponent)
    }

    pub(crate) fn get(&self) -> usize {
        self.0
    }
}
