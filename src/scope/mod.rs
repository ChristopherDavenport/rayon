use job::{Job, JobMode, JobRef};
use std::any::Any;
use std::cell::UnsafeCell;
use std::marker::PhantomData;
use std::mem;
use std::ptr;
use std::sync::atomic::{AtomicUsize, AtomicPtr, Ordering};
use std::sync::{Condvar, Mutex};
use thread_pool::{self, WorkerThread};
use unwind;

#[cfg(test)]
mod test;

pub struct Scope<'scope> {
    /// number of jobs created that have not yet completed or errored
    counter: AtomicUsize,

    /// if `#[cfg(debug_assertions)]`, then this counter is
    /// incremented in `HeapJob::new` and decremented in
    /// `HeapJob::drop`. When the scope is closed we assert that it is
    /// zero. It is used to check that we are freeing every heap job.
    leak_counter: AtomicUsize,

    /// if some job panicked, the error is stored here; it will be
    /// propagated to the one who created the scope
    panic: AtomicPtr<Box<Any + Send + 'static>>,

    /// mutex that is used in conjunction with `job_completed_cvar` below
    mutex: Mutex<()>,

    /// condition variable that is notified whenever jobs complete;
    /// used to block while waiting for jobs to complete
    job_completed_cvar: Condvar,

    marker: PhantomData<fn(&'scope ())>,
}

/// Create a "fork-join" scope `s` and invokes the closure with a
/// reference to `s`. This closure can then spawn asynchronous tasks
/// into `s`. Those tasks may run asynchronously with respect to the
/// closure; they may themselves spawn additional tasks into `s`. When
/// the closure returns, it will block until all tasks that have been
/// spawned into `s` complete.
///
/// `scope()` is a more flexible building block compared to `join()`,
/// since a loop can be used to spawn any number of tasks without
/// recursing. However, that flexibility comes at a performance price:
/// tasks spawned using `scope()` must be allocated onto the heap,
/// whereas `join()` can make exclusive use of the stack. **Prefer
/// `join()` (or, even better, parallel iterators) where possible.**
///
/// ### Example
///
/// The Rayon `join()` function launches two closures and waits for them
/// to stop. One could implement `join()` using a scope like so, although
/// it would be less efficient than the real implementation:
///
/// ```rust
/// pub fn join<A,B,RA,RB>(oper_a: A, oper_b: B) -> (RA, RB)
///     where A: FnOnce() -> RA + Send,
///           B: FnOnce() -> RB + Send,
///           RA: Send,
///           RB: Send,
/// {
///     let mut result_a: Option<RA> = None;
///     let mut result_b: Option<RB> = None;
///     rayon::scope(|s| {
///         s.spawn(|_| result_a = Some(oper_a()));
///         s.spawn(|_| result_b = Some(oper_b()));
///     });
///     (result_a.unwrap(), result_b.unwrap())
/// }
/// ```
///
/// ### Task execution
///
/// To see how and when tasks are joined, consider this example:
///
/// ```rust
/// // point start
/// rayon::scope(|s| {
///     s.spawn(|s| { // task s.1
///         s.spawn(|s| { // task s.1.1
///             rayon::scope(|t| {
///                 t.spawn(|_| ()); // task t.1
///                 t.spawn(|_| ()); // task t.2
///             });
///         });
///     });
///     s.spawn(|s| { // task 2
///     });
///     // point mid
/// });
/// // point end
/// ```
///
/// The various tasks that are run will execute roughly like so:
///
/// ```notrust
/// | (start)
/// |
/// | (scope `s` created)
/// +--------------------+ (task s.1)
/// +-------+ (task s.2) |
/// |       |            +---+ (task s.1.1)
/// |       |            |   |
/// |       |            |   | (scope `t` created)
/// |       |            |   +----------------+ (task t.1)
/// |       |            |   +---+ (task t.2) |
/// | (mid) |            |   |   |            |
/// :       |            |   + <-+------------+ (scope `t` ends)
/// :       |            |   |
/// |<------+------------+---+ (scope `s` ends)
/// |
/// | (end)
/// ```
///
/// The point here is that everything spawned into scope `s` will
/// terminate (at latest) at the same point -- right before the
/// original call to `rayon::scope` returns. This includes new
/// subtasks created by other subtasks (e.g., task `s.1.1`). If a new
/// scope is created (such as `t`), the things spawned into that scope
/// will be joined before that scope returns, which in turn occurs
/// before the creating task (task `s.1.1` in this case) finishes.
///
/// ### Accessing stack data
///
/// In general, spawned tasks may access stack data in place that
/// outlives the scope itself. Other data must be fully owned by the
/// spawned task.
///
/// ```rust
/// let ok: Vec<i32> = vec![1, 2, 3];
/// rayon::scope(|s| {
///     let bad: Vec<i32> = vec![4, 5, 6];
///     s.spawn(|_| {
///         // We can access `ok` because outlives the scope `s`.
///         println!("ok: {:?}", ok);
///
///         // If we just try to use `bad` here, the closure will borrow `bad`
///         // (because we are just printing it out, and that only requires a
///         // borrow), which will result in a compilation error. Read on
///         // for options.
///         // println!("bad: {:?}", bad);
///    });
/// });
/// ```
///
/// As the comments example above suggest, to reference `bad` we must
/// take ownership of it. One way to do this is to detach the closure
/// from the surrounding stack frame, using the `move` keyword. This
/// will cause it to take ownership of *all* the variables it touches,
/// in this case including both `ok` *and* `bad`:
///
/// ```rust
/// let ok: Vec<i32> = vec![1, 2, 3];
/// rayon::scope(|s| {
///     let bad: Vec<i32> = vec![4, 5, 6];
///     s.spawn(move |_| {
///         println!("ok: {:?}", ok);
///         println!("bad: {:?}", bad);
///     });
///
///     // That closure is fine, but now we can't use `ok` anywhere else,
///     // since it is owend by the previous task:
///     // s.spawn(|_| println!("ok: {:?}", ok));
/// });
/// ```
///
/// While this works, it could be a problem if we want to use `ok` elsewhere.
/// There are two choices. We can keep the closure as a `move` closure, but
/// instead of referencing the variable `ok`, we create a shadowed variable that
/// is a borrow of `ok` and capture *that*:
///
/// ```rust
/// let ok: Vec<i32> = vec![1, 2, 3];
/// rayon::scope(|s| {
///     let bad: Vec<i32> = vec![4, 5, 6];
///     let ok: &Vec<i32> = &ok; // shadow the original `ok`
///     s.spawn(move |_| {
///         println!("ok: {:?}", ok); // captures the shadowed version
///         println!("bad: {:?}", bad);
///     });
///
///     // Now we too can use the shadowed `ok`, since `&Vec<i32>` references
///     // can be shared freely. Note that we need a `move` closure here though,
///     // because otherwise we'd be trying to borrow the shadowed `ok`,
///     // and that doesn't outlive `scope`.
///     s.spawn(move |_| println!("ok: {:?}", ok));
/// });
/// ```
///
/// Another option is not to use the `move` keyword but instead to take ownership
/// of individual variables:
///
/// ```rust
/// let ok: Vec<i32> = vec![1, 2, 3];
/// rayon::scope(|s| {
///     let bad: Vec<i32> = vec![4, 5, 6];
///     s.spawn(|_| {
///         // Transfer ownership of `bad` into a local variable (also named `bad`).
///         // This will force the closure to take ownership of `bad` from the environment.
///         let bad = bad;
///         println!("ok: {:?}", ok); // `ok` is only borrowed.
///         println!("bad: {:?}", bad); // refers to our local variable, above.
///     });
///
///     s.spawn(|_| println!("ok: {:?}", ok)); // we too can borrow `ok`
/// });
/// ```
pub fn scope<'scope, OP, R>(op: OP) -> R
    where OP: for<'s> FnOnce(&'s Scope<'scope>) -> R
{
    let scope = Scope {
        counter: AtomicUsize::new(1),
        leak_counter: AtomicUsize::new(0),
        panic: AtomicPtr::new(ptr::null_mut()),
        mutex: Mutex::new(()),
        job_completed_cvar: Condvar::new(),
        marker: PhantomData,
    };
    if false { scope.fool_dead_code(); }
    let result = op(&scope);
    scope.job_completed_ok(); // `op` counts as a job
    scope.block_till_jobs_complete();
    result
}

impl<'scope> Scope<'scope> {
    /// just here to full the dead_code lint
    fn fool_dead_code(&self) {
        self.leak_counter.fetch_add(1, Ordering::SeqCst);
    }

    /// Spawns a job into the fork-join scope `self`. This job will
    /// execute sometime before the fork-join scope completes.  The
    /// job is specified as a closure, and this closure receives its
    /// own reference to `self` as argument. This can be used to
    /// inject new jobs into `self`.
    pub fn spawn<BODY>(&self, body: BODY)
        where BODY: FnOnce(&Scope<'scope>) + 'scope
    {
        unsafe {
            let old_value = self.counter.fetch_add(1, Ordering::SeqCst);
            assert!(old_value > 0); // scope can't have completed yet
            let job_ref = Box::new(HeapJob::new(self, body)).as_job_ref();
            let worker_thread = WorkerThread::current();
            if !worker_thread.is_null() {
                let worker_thread = &*worker_thread;
                let spawn_count = worker_thread.spawn_count();
                spawn_count.set(spawn_count.get() + 1);
                worker_thread.push(job_ref);
            } else {
                thread_pool::get_registry().inject(&[job_ref]);
            }
        }
    }

    fn job_panicked(&self, err: Box<Any + Send + 'static>) {
        // capture the first error we see, free the rest
        let nil = ptr::null_mut();
        let mut err = Box::new(err); // box up the fat ptr
        if self.panic.compare_and_swap(nil, &mut *err, Ordering::SeqCst).is_null() {
            mem::forget(err); // ownership now transferred into self.panic
        }

        self.job_completed_ok()
    }

    fn job_completed_ok(&self) {
        let old_value = self.counter.fetch_sub(1, Ordering::Release);
        if old_value == 1 {
            // Important: grab the lock here to avoid a data race with
            // the `block_till_jobs_complete` code. Consider what could
            // otherwise happen:
            //
            // ```
            //    Us          Them
            //              Acquire lock
            //              Read counter: 1
            // Dec counter
            // Notify all
            //              Wait on job_completed_cvar
            // ```
            //
            // By holding the lock, we ensure that the "read counter"
            // and "wait on job_completed_cvar" occur atomically with respect to the
            // notify.
            let _guard = self.mutex.lock().unwrap();
            self.job_completed_cvar.notify_all();
        }
    }

    fn block_till_jobs_complete(&self) {
        // wait for job counter to reach 0:
        //
        // FIXME -- if on a worker thread, we should be helping here
        let mut guard = self.mutex.lock().unwrap();
        while self.counter.load(Ordering::Acquire) > 0 {
            guard = self.job_completed_cvar.wait(guard).unwrap();
        }

        // propagate panic, if any occurred; at this point, all
        // outstanding jobs have completed, so we can use a relaxed
        // ordering:
        let panic = self.panic.swap(ptr::null_mut(), Ordering::Relaxed);
        if !panic.is_null() {
            unsafe {
                let value: Box<Box<Any + Send + 'static>> = mem::transmute(panic);
                unwind::resume_unwinding(*value);
            }
        }
    }

    #[cfg(debug_assertions)]
    fn job_created(&self) {
        self.leak_counter.fetch_add(1, Ordering::SeqCst);
    }

    #[cfg(not(debug_assertions))]
    fn job_created(&self) {
    }

    #[cfg(debug_assertions)]
    fn job_dropped(&self) {
        self.leak_counter.fetch_sub(1, Ordering::SeqCst);
    }
}

struct HeapJob<'scope, BODY>
    where BODY: FnOnce(&Scope<'scope>) + 'scope,
{
    scope: *const Scope<'scope>,
    func: UnsafeCell<Option<BODY>>,
}

impl<'scope, BODY> HeapJob<'scope, BODY>
    where BODY: FnOnce(&Scope<'scope>) + 'scope
{
    fn new(scope: *const Scope<'scope>, func: BODY) -> Self {
        unsafe { (*scope).job_created(); }
        HeapJob {
            scope: scope,
            func: UnsafeCell::new(Some(func))
        }
    }

    unsafe fn as_job_ref(self: Box<Self>) -> JobRef {
        let this: *const Self = mem::transmute(self);
        JobRef::new(this)
    }

    /// We have to maintain an invariant that we pop off any work
    /// that we pushed onto the local thread deque. In other words,
    /// if no thieves are at play, then the height of the local
    /// deque must be the same when we enter and exit. Otherwise,
    /// we get into trouble composing with the main `join` API,
    /// which assumes that -- after it executes the first closure
    /// -- the top-most thing on the stack is the second closure.
    unsafe fn pop_jobs(worker_thread: &WorkerThread, start_count: usize) {
        let spawn_count = worker_thread.spawn_count();
        let current_count = spawn_count.get();
        for _ in start_count .. current_count {
            if let Some(job_ref) = worker_thread.pop() {
                job_ref.execute(JobMode::Execute);
            }
        }
        spawn_count.set(start_count);
    }
}

impl<'scope, BODY> Job for HeapJob<'scope, BODY>
    where BODY: FnOnce(&Scope<'scope>) + 'scope
{
    unsafe fn execute(this: *const Self, mode: JobMode) {
        let this: &Self = mem::transmute(this); // FIXME
        let scope = &*this.scope;

        match mode {
            JobMode::Execute => {
                let worker_thread = &*WorkerThread::current();
                let start_count = worker_thread.spawn_count().get();

                let func = (*this.func.get()).take().unwrap();
                match unwind::halt_unwinding(|| func(&*scope)) {
                    Ok(()) => { (*scope).job_completed_ok(); }
                    Err(err) => { (*scope).job_panicked(err); }
                }

                Self::pop_jobs(worker_thread, start_count);
            }

            JobMode::Abort => {
                (*this.scope).job_completed_ok();
            }
        }
    }
}

#[cfg(debug_assertions)]
impl<'scope, BODY> Drop for HeapJob<'scope, BODY>
    where BODY: FnOnce(&Scope<'scope>) + 'scope
{
    fn drop(&mut self) {
        println!("Foo");
        unsafe {
            (*self.scope).job_dropped();
        }
    }
}
