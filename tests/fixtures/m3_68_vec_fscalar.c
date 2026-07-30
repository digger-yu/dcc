typedef float v4f __attribute__((vector_size(16)));
typedef double v2d __attribute__((vector_size(16)));

int main(void) {
	v4f fa = {1.0f, 2.0f, 3.0f, 4.0f};
	v4f fb = {10.0f, 20.0f, 30.0f, 40.0f};
	v4f fc = fa + fb;
	v4f fd = fa * (v4f){2.0f, 2.0f, 2.0f, 2.0f};
	float *pf = (float *)&fc;
	if (pf[0] != 11.0f || pf[3] != 44.0f)
		return 1;
	pf = (float *)&fd;
	if (pf[1] != 4.0f || pf[2] != 6.0f)
		return 2;

	v2d da = {1.5, 2.5};
	v2d db = {0.5, 1.5};
	v2d dc = da - db;
	double *pd = (double *)&dc;
	if (pd[0] != 1.0 || pd[1] != 1.0)
		return 3;
	return 0;
}
