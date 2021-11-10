#include <unistd.h>
#include <iostream>

void foo() {
  if (rand() % 10) {
    // Fast path
    usleep(1000);
  } else {
    // Slow path
    usleep(10000);
  }
}

void bar() {
  for (int i = 0; i < 1000000; ++i) {}
}

void work(bool call_foo) {
  if (call_foo) {
    foo();
  } else {
    bar();
  }
}

int main() {
  for (int i = 0; ; ++i) {
    work(i % 2);
  }
  return 0;
}
