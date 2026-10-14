//! Fixed-size sample history for the performance graphs.

/// Ring buffer of the most recent `capacity` samples.
#[derive(Clone, Debug)]
pub struct Ring {
    data: Vec<f32>,
    head: usize,
    len: usize,
}

impl Ring {
    pub fn new(capacity: usize) -> Self {
        Self {
            data: vec![0.0; capacity.max(1)],
            head: 0,
            len: 0,
        }
    }

    pub fn push(&mut self, value: f32) {
        self.data[self.head] = value;
        self.head = (self.head + 1) % self.data.len();
        self.len = (self.len + 1).min(self.data.len());
    }

    pub fn capacity(&self) -> usize {
        self.data.len()
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn last(&self) -> Option<f32> {
        (self.len > 0).then(|| self.data[(self.head + self.data.len() - 1) % self.data.len()])
    }

    /// Oldest to newest.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = f32> + '_ {
        let cap = self.data.len();
        let start = (self.head + cap - self.len) % cap;
        (0..self.len).map(move |i| self.data[(start + i) % cap])
    }

    pub fn max(&self) -> f32 {
        self.iter().fold(0.0, f32::max)
    }
}

#[derive(Clone, Debug)]
pub struct GpuHistory {
    /// Busiest engine.
    pub usage: Ring,
    pub engines: Vec<Ring>,
    pub dedicated: Ring,
}

#[derive(Clone, Debug)]
pub struct DiskHistory {
    pub number: u32,
    pub read: Ring,
    pub write: Ring,
    pub active: Ring,
}

#[derive(Clone, Debug)]
pub struct AdapterHistory {
    pub luid: u64,
    pub rx: Ring,
    pub tx: Ring,
}

#[derive(Clone, Debug)]
pub struct History {
    pub cpu: Ring,
    pub cpu_kernel: Ring,
    pub cores: Vec<Ring>,
    pub core_kernel: Vec<Ring>,
    pub memory_used: Ring,
    pub commit: Ring,
    pub disk_read: Ring,
    pub disk_write: Ring,
    pub io_read: Ring,
    pub io_write: Ring,
    pub net_rx: Ring,
    pub net_tx: Ring,
    pub gpus: Vec<GpuHistory>,
    /// Devices come and go (USB disks, Wi-Fi), so these grow on demand.
    pub disks: Vec<DiskHistory>,
    pub adapters: Vec<AdapterHistory>,
}

impl History {
    /// `gpu_engines` holds the engine count of each GPU.
    pub fn new(capacity: usize, cores: usize, gpu_engines: &[usize]) -> Self {
        Self {
            cpu: Ring::new(capacity),
            cpu_kernel: Ring::new(capacity),
            cores: (0..cores).map(|_| Ring::new(capacity)).collect(),
            core_kernel: (0..cores).map(|_| Ring::new(capacity)).collect(),
            memory_used: Ring::new(capacity),
            commit: Ring::new(capacity),
            disk_read: Ring::new(capacity),
            disk_write: Ring::new(capacity),
            io_read: Ring::new(capacity),
            io_write: Ring::new(capacity),
            net_rx: Ring::new(capacity),
            net_tx: Ring::new(capacity),
            disks: Vec::new(),
            adapters: Vec::new(),
            gpus: gpu_engines
                .iter()
                .map(|&engines| GpuHistory {
                    usage: Ring::new(capacity),
                    engines: (0..engines).map(|_| Ring::new(capacity)).collect(),
                    dedicated: Ring::new(capacity),
                })
                .collect(),
        }
    }
}

impl History {
    pub fn disk(&self, number: u32) -> Option<&DiskHistory> {
        self.disks.iter().find(|d| d.number == number)
    }

    pub fn disk_mut(&mut self, number: u32) -> &mut DiskHistory {
        let capacity = self.cpu.capacity();
        match self.disks.iter().position(|d| d.number == number) {
            Some(i) => &mut self.disks[i],
            None => {
                self.disks.push(DiskHistory {
                    number,
                    read: Ring::new(capacity),
                    write: Ring::new(capacity),
                    active: Ring::new(capacity),
                });
                self.disks.last_mut().unwrap()
            }
        }
    }

    pub fn adapter(&self, luid: u64) -> Option<&AdapterHistory> {
        self.adapters.iter().find(|a| a.luid == luid)
    }

    pub fn adapter_mut(&mut self, luid: u64) -> &mut AdapterHistory {
        let capacity = self.cpu.capacity();
        match self.adapters.iter().position(|a| a.luid == luid) {
            Some(i) => &mut self.adapters[i],
            None => {
                self.adapters.push(AdapterHistory {
                    luid,
                    rx: Ring::new(capacity),
                    tx: Ring::new(capacity),
                });
                self.adapters.last_mut().unwrap()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Ring;

    #[test]
    fn ring_wraps_oldest_first() {
        let mut r = Ring::new(3);
        assert!(r.is_empty());
        for v in 1..=5 {
            r.push(v as f32);
        }
        assert_eq!(r.iter().collect::<Vec<_>>(), vec![3.0, 4.0, 5.0]);
        assert_eq!(r.last(), Some(5.0));
        assert_eq!(r.max(), 5.0);
    }
}
