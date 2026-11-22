/* Minimal pthread shim for building libuvc with MSVC.
 * Covers exactly what libuvc uses: create/join, mutex, condition variable (incl. timedwait).
 * Mutexes are SRW locks and condition variables are native CONDITION_VARIABLEs. */
#ifndef THERMALVIEW_PTHREAD_SHIM_H
#define THERMALVIEW_PTHREAD_SHIM_H

#include <windows.h>
#include <process.h>
#include <stdlib.h>
#include <time.h>
#include <errno.h>

#ifndef ETIMEDOUT
#define ETIMEDOUT 138
#endif

/* stream.c's MSVC gettimeofday() takes a struct timezone pointer; it is never dereferenced. */
struct timezone;

typedef HANDLE pthread_t;
typedef SRWLOCK pthread_mutex_t;
typedef CONDITION_VARIABLE pthread_cond_t;
typedef int pthread_attr_t;
typedef int pthread_mutexattr_t;
typedef int pthread_condattr_t;

struct tv_pthread_start {
  void *(*fn)(void *);
  void *arg;
};

static unsigned __stdcall tv_pthread_trampoline(void *p) {
  struct tv_pthread_start s = *(struct tv_pthread_start *)p;
  free(p);
  s.fn(s.arg);
  return 0;
}

static __inline int pthread_create(pthread_t *t, const pthread_attr_t *attr, void *(*fn)(void *), void *arg) {
  struct tv_pthread_start *s;
  uintptr_t h;
  (void)attr;
  s = (struct tv_pthread_start *)malloc(sizeof *s);
  if (!s) return ENOMEM;
  s->fn = fn;
  s->arg = arg;
  h = _beginthreadex(NULL, 0, tv_pthread_trampoline, s, 0, NULL);
  if (h == 0) { free(s); return EAGAIN; }
  *t = (HANDLE)h;
  return 0;
}

static __inline int pthread_join(pthread_t t, void **ret) {
  if (ret) *ret = NULL;
  if (WaitForSingleObject(t, INFINITE) != WAIT_OBJECT_0) return EINVAL;
  CloseHandle(t);
  return 0;
}

static __inline int pthread_mutex_init(pthread_mutex_t *m, const pthread_mutexattr_t *a) { (void)a; InitializeSRWLock(m); return 0; }
static __inline int pthread_mutex_destroy(pthread_mutex_t *m) { (void)m; return 0; }
static __inline int pthread_mutex_lock(pthread_mutex_t *m) { AcquireSRWLockExclusive(m); return 0; }
static __inline int pthread_mutex_unlock(pthread_mutex_t *m) { ReleaseSRWLockExclusive(m); return 0; }

static __inline int pthread_cond_init(pthread_cond_t *c, const pthread_condattr_t *a) { (void)a; InitializeConditionVariable(c); return 0; }
static __inline int pthread_cond_destroy(pthread_cond_t *c) { (void)c; return 0; }
static __inline int pthread_cond_signal(pthread_cond_t *c) { WakeConditionVariable(c); return 0; }
static __inline int pthread_cond_broadcast(pthread_cond_t *c) { WakeAllConditionVariable(c); return 0; }

static __inline int pthread_cond_wait(pthread_cond_t *c, pthread_mutex_t *m) {
  return SleepConditionVariableSRW(c, m, INFINITE, 0) ? 0 : EINVAL;
}

/* abstime is wall-clock time since the Unix epoch (what libuvc computes from gettimeofday). */
static __inline int pthread_cond_timedwait(pthread_cond_t *c, pthread_mutex_t *m, const struct timespec *abstime) {
  FILETIME ft;
  ULARGE_INTEGER now100ns;
  long long now_ms, deadline_ms, wait_ms;
  GetSystemTimeAsFileTime(&ft);
  now100ns.LowPart = ft.dwLowDateTime;
  now100ns.HighPart = ft.dwHighDateTime;
  now_ms = (long long)((now100ns.QuadPart - 116444736000000000ULL) / 10000ULL);
  deadline_ms = (long long)abstime->tv_sec * 1000 + abstime->tv_nsec / 1000000;
  wait_ms = deadline_ms - now_ms;
  if (wait_ms <= 0) return ETIMEDOUT;
  if (SleepConditionVariableSRW(c, m, (DWORD)wait_ms, 0)) return 0;
  return GetLastError() == ERROR_TIMEOUT ? ETIMEDOUT : EINVAL;
}

#endif
