typedef int (pcpu_fc_cpu_to_node_fn_t)(int cpu);
typedef int (pcpu_fc_cpu_distance_fn_t)(unsigned int from, unsigned int to);

int use(pcpu_fc_cpu_to_node_fn_t fn) { return fn(0); }
