int main(void) {
    volatile unsigned long *null_ptr = (volatile unsigned long *)0;
    return (int)*null_ptr;
}
