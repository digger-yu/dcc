struct pcpu_hot {
	int dummy;
};

extern __attribute__((section(".data..percpu" "..shared_aligned"))) __typeof__(struct pcpu_hot) pcpu_hot __attribute__((__aligned__((1 << (6)))));

int use(void) { return pcpu_hot.dummy; }
