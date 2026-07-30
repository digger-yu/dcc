unsigned long sysctl_vfs_cache_pressure = 100;

static inline unsigned long vfs_pressure_ratio(unsigned long val)
{
	return ({ typeof(val) x_ = (val); typeof(sysctl_vfs_cache_pressure) n_ = (sysctl_vfs_cache_pressure); typeof(100) d_ = (100); typeof(x_) q = x_ / d_; typeof(x_) r = x_ % d_; q * n_ + r * n_ / d_; });
}

int main(void) { return vfs_pressure_ratio(50); }
