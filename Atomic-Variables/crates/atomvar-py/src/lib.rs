//! Python bindings for atomvar.
//!
//! Every operation runs with the GIL held (on GIL builds): the atomic op costs
//! nanoseconds, far less than releasing and re-acquiring the GIL would. The
//! module declares `gil_used = false`, which is a thread-safety declaration for
//! free-threaded CPython, not a GIL-release mechanism.

use atomvar_core::{self as core, Error, Ordering};
use pyo3::create_exception;
use pyo3::exceptions::{PyException, PyOSError, PyValueError};
use pyo3::prelude::*;

create_exception!(atomvar, AtomvarError, PyException, "Base class for atomvar errors.");
create_exception!(atomvar, InvalidOrderingError, AtomvarError, "Memory ordering not valid for this operation.");
create_exception!(atomvar, InvalidNameError, AtomvarError, "Arena or variable name is invalid.");
create_exception!(atomvar, ArenaFullError, AtomvarError, "The arena has no free slots.");
create_exception!(atomvar, TypeMismatchError, AtomvarError, "The variable exists with a different type.");
create_exception!(atomvar, NotFoundError, AtomvarError, "The variable does not exist.");
create_exception!(atomvar, LayoutMismatchError, AtomvarError, "Existing arena has a different capacity or layout.");
create_exception!(atomvar, ForkedProcessError, AtomvarError, "Used in a plain fork() child; use spawn or forkserver.");
create_exception!(atomvar, UnsupportedCpuError, AtomvarError, "CPU lacks CMPXCHG16B.");

fn err(e: Error) -> PyErr {
    let msg = e.to_string();
    match e {
        Error::InvalidOrdering(_) => InvalidOrderingError::new_err(msg),
        Error::InvalidName(_) => InvalidNameError::new_err(msg),
        Error::InvalidArgument(_) => PyValueError::new_err(msg),
        Error::ArenaFull { .. } => ArenaFullError::new_err(msg),
        Error::TypeMismatch { .. } => TypeMismatchError::new_err(msg),
        Error::NotFound { .. } => NotFoundError::new_err(msg),
        Error::LayoutMismatch(_) => LayoutMismatchError::new_err(msg),
        Error::ForkedProcess => ForkedProcessError::new_err(msg),
        Error::UnsupportedCpu => UnsupportedCpuError::new_err(msg),
        Error::Io(_) => PyOSError::new_err(msg),
    }
}

/// Memory ordering. SEQ_CST (the default everywhere) matches Java AtomicLong.
#[pyclass(eq, eq_int, frozen, from_py_object, module = "atomvar", name = "Ordering")]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PyOrdering {
    #[pyo3(name = "RELAXED")]
    Relaxed = 0,
    #[pyo3(name = "ACQUIRE")]
    Acquire = 1,
    #[pyo3(name = "RELEASE")]
    Release = 2,
    #[pyo3(name = "ACQ_REL")]
    AcqRel = 3,
    #[pyo3(name = "SEQ_CST")]
    SeqCst = 4,
}

impl From<PyOrdering> for Ordering {
    fn from(o: PyOrdering) -> Self {
        match o {
            PyOrdering::Relaxed => Ordering::Relaxed,
            PyOrdering::Acquire => Ordering::Acquire,
            PyOrdering::Release => Ordering::Release,
            PyOrdering::AcqRel => Ordering::AcqRel,
            PyOrdering::SeqCst => Ordering::SeqCst,
        }
    }
}

fn failure_or_default(success: PyOrdering, failure: Option<PyOrdering>) -> Ordering {
    failure.map(Ordering::from).unwrap_or_else(|| Ordering::from(success).default_failure())
}

fn flatten<T>(r: std::result::Result<T, T>) -> (bool, T) {
    match r {
        Ok(v) => (true, v),
        Err(v) => (false, v),
    }
}

const SC: PyOrdering = PyOrdering::SeqCst;

// ---------------------------------------------------------------------------
// Integer handles
// ---------------------------------------------------------------------------

macro_rules! py_int {
    ($Py:ident, $pyname:literal, $Core:ident, $t:ty, { $($extra:tt)* }) => {
        /// Lock-free atomic integer. Arithmetic wraps on overflow (like Java).
        #[pyclass(frozen, module = "atomvar", name = $pyname)]
        pub struct $Py(core::$Core);

        #[pymethods]
        impl $Py {
            #[new]
            #[pyo3(signature = (value = 0))]
            fn new(value: $t) -> Self {
                Self(core::$Core::new(value))
            }

            /// Current value (SEQ_CST read).
            #[getter]
            fn value(&self) -> $t {
                self.0.load(Ordering::SeqCst).expect("seq_cst load")
            }

            #[pyo3(signature = (order = SC))]
            fn get(&self, order: PyOrdering) -> PyResult<$t> {
                self.0.load(order.into()).map_err(err)
            }

            #[pyo3(signature = (order = SC))]
            fn load(&self, order: PyOrdering) -> PyResult<$t> {
                self.get(order)
            }

            #[pyo3(signature = (value, order = SC))]
            fn set(&self, value: $t, order: PyOrdering) -> PyResult<()> {
                self.0.store(value, order.into()).map_err(err)
            }

            #[pyo3(signature = (value, order = SC))]
            fn store(&self, value: $t, order: PyOrdering) -> PyResult<()> {
                self.set(value, order)
            }

            /// Sets the value and returns the previous one.
            #[pyo3(signature = (value, order = SC))]
            fn get_and_set(&self, value: $t, order: PyOrdering) -> $t {
                self.0.swap(value, order.into())
            }

            #[pyo3(signature = (value, order = SC))]
            fn swap(&self, value: $t, order: PyOrdering) -> $t {
                self.0.swap(value, order.into())
            }

            /// Java-style CAS: sets `update` if the current value is `expect`.
            fn compare_and_set(&self, expect: $t, update: $t) -> bool {
                self.0
                    .compare_exchange(expect, update, Ordering::SeqCst, Ordering::SeqCst)
                    .expect("seq_cst pair")
                    .is_ok()
            }

            /// Returns (exchanged, previous_value).
            #[pyo3(signature = (expected, desired, success = SC, failure = None))]
            fn compare_exchange(
                &self,
                expected: $t,
                desired: $t,
                success: PyOrdering,
                failure: Option<PyOrdering>,
            ) -> PyResult<(bool, $t)> {
                let f = failure_or_default(success, failure);
                Ok(flatten(self.0.compare_exchange(expected, desired, success.into(), f).map_err(err)?))
            }

            /// Like compare_exchange but may fail spuriously; use in retry loops.
            #[pyo3(signature = (expected, desired, success = SC, failure = None))]
            fn compare_exchange_weak(
                &self,
                expected: $t,
                desired: $t,
                success: PyOrdering,
                failure: Option<PyOrdering>,
            ) -> PyResult<(bool, $t)> {
                let f = failure_or_default(success, failure);
                Ok(flatten(self.0.compare_exchange_weak(expected, desired, success.into(), f).map_err(err)?))
            }

            #[pyo3(signature = (value, order = SC))]
            fn fetch_add(&self, value: $t, order: PyOrdering) -> $t {
                self.0.fetch_add(value, order.into())
            }
            #[pyo3(signature = (value, order = SC))]
            fn fetch_sub(&self, value: $t, order: PyOrdering) -> $t {
                self.0.fetch_sub(value, order.into())
            }
            #[pyo3(signature = (value, order = SC))]
            fn fetch_and(&self, value: $t, order: PyOrdering) -> $t {
                self.0.fetch_and(value, order.into())
            }
            #[pyo3(signature = (value, order = SC))]
            fn fetch_or(&self, value: $t, order: PyOrdering) -> $t {
                self.0.fetch_or(value, order.into())
            }
            #[pyo3(signature = (value, order = SC))]
            fn fetch_xor(&self, value: $t, order: PyOrdering) -> $t {
                self.0.fetch_xor(value, order.into())
            }
            #[pyo3(signature = (value, order = SC))]
            fn fetch_max(&self, value: $t, order: PyOrdering) -> $t {
                self.0.fetch_max(value, order.into())
            }
            #[pyo3(signature = (value, order = SC))]
            fn fetch_min(&self, value: $t, order: PyOrdering) -> $t {
                self.0.fetch_min(value, order.into())
            }
            #[pyo3(signature = (value, order = SC))]
            fn get_and_add(&self, value: $t, order: PyOrdering) -> $t {
                self.0.fetch_add(value, order.into())
            }
            #[pyo3(signature = (value, order = SC))]
            fn add_and_get(&self, value: $t, order: PyOrdering) -> $t {
                self.0.add_and_get(value, order.into())
            }
            #[pyo3(signature = (value, order = SC))]
            fn sub_and_get(&self, value: $t, order: PyOrdering) -> $t {
                self.0.sub_and_get(value, order.into())
            }
            #[pyo3(signature = (order = SC))]
            fn increment_and_get(&self, order: PyOrdering) -> $t {
                self.0.increment_and_get(order.into())
            }
            #[pyo3(signature = (order = SC))]
            fn decrement_and_get(&self, order: PyOrdering) -> $t {
                self.0.decrement_and_get(order.into())
            }
            #[pyo3(signature = (order = SC))]
            fn get_and_increment(&self, order: PyOrdering) -> $t {
                self.0.fetch_add(1, order.into())
            }
            #[pyo3(signature = (order = SC))]
            fn get_and_decrement(&self, order: PyOrdering) -> $t {
                self.0.fetch_sub(1, order.into())
            }

            fn __repr__(&self) -> String {
                format!("{}({})", $pyname, self.value())
            }

            $($extra)*
        }
    };
}

py_int!(PyAtomicInt, "AtomicInt", AtomicI64, i64, {
    /// Benchmark-only: fetch_add with the GIL released around the op. Exists to
    /// measure the cost of detaching (see benches/python_gil.py).
    fn _fetch_add_detached(&self, py: Python<'_>, value: i64) -> i64 {
        py.detach(|| self.0.fetch_add(value, Ordering::SeqCst))
    }
});
py_int!(PyAtomicUInt, "AtomicUInt", AtomicU64, u64, {});

// ---------------------------------------------------------------------------
// Bool
// ---------------------------------------------------------------------------

/// Lock-free atomic bool.
#[pyclass(frozen, module = "atomvar", name = "AtomicBool")]
pub struct PyAtomicBool(core::AtomicBool);

#[pymethods]
impl PyAtomicBool {
    #[new]
    #[pyo3(signature = (value = false))]
    fn new(value: bool) -> Self {
        Self(core::AtomicBool::new(value))
    }
    #[getter]
    fn value(&self) -> bool {
        self.0.load(Ordering::SeqCst).expect("seq_cst load")
    }
    #[pyo3(signature = (order = SC))]
    fn get(&self, order: PyOrdering) -> PyResult<bool> {
        self.0.load(order.into()).map_err(err)
    }
    #[pyo3(signature = (order = SC))]
    fn load(&self, order: PyOrdering) -> PyResult<bool> {
        self.get(order)
    }
    #[pyo3(signature = (value, order = SC))]
    fn set(&self, value: bool, order: PyOrdering) -> PyResult<()> {
        self.0.store(value, order.into()).map_err(err)
    }
    #[pyo3(signature = (value, order = SC))]
    fn store(&self, value: bool, order: PyOrdering) -> PyResult<()> {
        self.set(value, order)
    }
    #[pyo3(signature = (value, order = SC))]
    fn get_and_set(&self, value: bool, order: PyOrdering) -> bool {
        self.0.swap(value, order.into())
    }
    #[pyo3(signature = (value, order = SC))]
    fn swap(&self, value: bool, order: PyOrdering) -> bool {
        self.0.swap(value, order.into())
    }
    fn compare_and_set(&self, expect: bool, update: bool) -> bool {
        self.0
            .compare_exchange(expect, update, Ordering::SeqCst, Ordering::SeqCst)
            .expect("seq_cst pair")
            .is_ok()
    }
    #[pyo3(signature = (expected, desired, success = SC, failure = None))]
    fn compare_exchange(
        &self,
        expected: bool,
        desired: bool,
        success: PyOrdering,
        failure: Option<PyOrdering>,
    ) -> PyResult<(bool, bool)> {
        let f = failure_or_default(success, failure);
        Ok(flatten(self.0.compare_exchange(expected, desired, success.into(), f).map_err(err)?))
    }
    #[pyo3(signature = (expected, desired, success = SC, failure = None))]
    fn compare_exchange_weak(
        &self,
        expected: bool,
        desired: bool,
        success: PyOrdering,
        failure: Option<PyOrdering>,
    ) -> PyResult<(bool, bool)> {
        let f = failure_or_default(success, failure);
        Ok(flatten(self.0.compare_exchange_weak(expected, desired, success.into(), f).map_err(err)?))
    }
    #[pyo3(signature = (value, order = SC))]
    fn fetch_and(&self, value: bool, order: PyOrdering) -> bool {
        self.0.fetch_and(value, order.into())
    }
    #[pyo3(signature = (value, order = SC))]
    fn fetch_or(&self, value: bool, order: PyOrdering) -> bool {
        self.0.fetch_or(value, order.into())
    }
    #[pyo3(signature = (value, order = SC))]
    fn fetch_xor(&self, value: bool, order: PyOrdering) -> bool {
        self.0.fetch_xor(value, order.into())
    }
    #[pyo3(signature = (value, order = SC))]
    fn fetch_nand(&self, value: bool, order: PyOrdering) -> bool {
        self.0.fetch_nand(value, order.into())
    }
    fn __bool__(&self) -> bool {
        self.value()
    }
    fn __repr__(&self) -> String {
        format!("AtomicBool({})", if self.value() { "True" } else { "False" })
    }
}

// ---------------------------------------------------------------------------
// Float
// ---------------------------------------------------------------------------

/// Lock-free atomic float (f64). compare_and_set compares exact bits: 0.0 and
/// -0.0 differ, and a NaN matches only the identical bit pattern.
#[pyclass(frozen, module = "atomvar", name = "AtomicFloat")]
pub struct PyAtomicFloat(core::AtomicF64);

#[pymethods]
impl PyAtomicFloat {
    #[new]
    #[pyo3(signature = (value = 0.0))]
    fn new(value: f64) -> Self {
        Self(core::AtomicF64::new(value))
    }
    #[getter]
    fn value(&self) -> f64 {
        self.0.load(Ordering::SeqCst).expect("seq_cst load")
    }
    #[pyo3(signature = (order = SC))]
    fn get(&self, order: PyOrdering) -> PyResult<f64> {
        self.0.load(order.into()).map_err(err)
    }
    #[pyo3(signature = (order = SC))]
    fn load(&self, order: PyOrdering) -> PyResult<f64> {
        self.get(order)
    }
    #[pyo3(signature = (value, order = SC))]
    fn set(&self, value: f64, order: PyOrdering) -> PyResult<()> {
        self.0.store(value, order.into()).map_err(err)
    }
    #[pyo3(signature = (value, order = SC))]
    fn store(&self, value: f64, order: PyOrdering) -> PyResult<()> {
        self.set(value, order)
    }
    #[pyo3(signature = (order = SC))]
    fn get_bits(&self, order: PyOrdering) -> PyResult<u64> {
        self.0.load_bits(order.into()).map_err(err)
    }
    #[pyo3(signature = (bits, order = SC))]
    fn set_bits(&self, bits: u64, order: PyOrdering) -> PyResult<()> {
        self.0.store_bits(bits, order.into()).map_err(err)
    }
    #[pyo3(signature = (value, order = SC))]
    fn get_and_set(&self, value: f64, order: PyOrdering) -> f64 {
        self.0.swap(value, order.into())
    }
    #[pyo3(signature = (value, order = SC))]
    fn swap(&self, value: f64, order: PyOrdering) -> f64 {
        self.0.swap(value, order.into())
    }
    /// Bitwise CAS on the float values.
    fn compare_and_set(&self, expect: f64, update: f64) -> bool {
        self.0
            .compare_exchange(expect, update, Ordering::SeqCst, Ordering::SeqCst)
            .expect("seq_cst pair")
            .is_ok()
    }
    fn compare_and_set_bits(&self, expect: u64, update: u64) -> bool {
        self.0
            .compare_exchange_bits(expect, update, Ordering::SeqCst, Ordering::SeqCst)
            .expect("seq_cst pair")
            .is_ok()
    }
    #[pyo3(signature = (expected, desired, success = SC, failure = None))]
    fn compare_exchange(
        &self,
        expected: f64,
        desired: f64,
        success: PyOrdering,
        failure: Option<PyOrdering>,
    ) -> PyResult<(bool, f64)> {
        let f = failure_or_default(success, failure);
        Ok(flatten(self.0.compare_exchange(expected, desired, success.into(), f).map_err(err)?))
    }
    /// IEEE add via CAS loop; returns the old value.
    #[pyo3(signature = (value, order = SC))]
    fn fetch_add(&self, value: f64, order: PyOrdering) -> f64 {
        self.0.fetch_add(value, order.into())
    }
    #[pyo3(signature = (value, order = SC))]
    fn fetch_sub(&self, value: f64, order: PyOrdering) -> f64 {
        self.0.fetch_sub(value, order.into())
    }
    #[pyo3(signature = (value, order = SC))]
    fn add_and_get(&self, value: f64, order: PyOrdering) -> f64 {
        self.0.add_and_get(value, order.into())
    }
    fn __float__(&self) -> f64 {
        self.value()
    }
    fn __repr__(&self) -> String {
        format!("AtomicFloat({:?})", self.value())
    }
}

// ---------------------------------------------------------------------------
// U128
// ---------------------------------------------------------------------------

/// Lock-free atomic 128-bit unsigned integer (CMPXCHG16B). Use it for
/// ABA-safe versioned values: pack (version << 64) | value and bump the
/// version on every write.
#[pyclass(frozen, module = "atomvar", name = "AtomicU128")]
pub struct PyAtomicU128(core::AtomicU128);

#[pymethods]
impl PyAtomicU128 {
    #[new]
    #[pyo3(signature = (value = 0))]
    fn new(value: u128) -> Self {
        Self(core::AtomicU128::new(value))
    }
    #[getter]
    fn value(&self) -> u128 {
        self.0.load(Ordering::SeqCst).expect("seq_cst load")
    }
    #[pyo3(signature = (order = SC))]
    fn get(&self, order: PyOrdering) -> PyResult<u128> {
        self.0.load(order.into()).map_err(err)
    }
    #[pyo3(signature = (order = SC))]
    fn load(&self, order: PyOrdering) -> PyResult<u128> {
        self.get(order)
    }
    #[pyo3(signature = (value, order = SC))]
    fn set(&self, value: u128, order: PyOrdering) -> PyResult<()> {
        self.0.store(value, order.into()).map_err(err)
    }
    #[pyo3(signature = (value, order = SC))]
    fn store(&self, value: u128, order: PyOrdering) -> PyResult<()> {
        self.set(value, order)
    }
    #[pyo3(signature = (value, order = SC))]
    fn get_and_set(&self, value: u128, order: PyOrdering) -> u128 {
        self.0.swap(value, order.into())
    }
    #[pyo3(signature = (value, order = SC))]
    fn swap(&self, value: u128, order: PyOrdering) -> u128 {
        self.0.swap(value, order.into())
    }
    fn compare_and_set(&self, expect: u128, update: u128) -> bool {
        self.0
            .compare_exchange(expect, update, Ordering::SeqCst, Ordering::SeqCst)
            .expect("seq_cst pair")
            .is_ok()
    }
    #[pyo3(signature = (expected, desired, success = SC, failure = None))]
    fn compare_exchange(
        &self,
        expected: u128,
        desired: u128,
        success: PyOrdering,
        failure: Option<PyOrdering>,
    ) -> PyResult<(bool, u128)> {
        let f = failure_or_default(success, failure);
        Ok(flatten(self.0.compare_exchange(expected, desired, success.into(), f).map_err(err)?))
    }
    #[pyo3(signature = (expected, desired, success = SC, failure = None))]
    fn compare_exchange_weak(
        &self,
        expected: u128,
        desired: u128,
        success: PyOrdering,
        failure: Option<PyOrdering>,
    ) -> PyResult<(bool, u128)> {
        let f = failure_or_default(success, failure);
        Ok(flatten(self.0.compare_exchange_weak(expected, desired, success.into(), f).map_err(err)?))
    }
    fn __repr__(&self) -> String {
        format!("AtomicU128({})", self.value())
    }
}

// ---------------------------------------------------------------------------
// Arena
// ---------------------------------------------------------------------------

/// A named shared-memory arena. Any process that opens the same name (in any
/// language) sees the same variables. Open arenas in spawned processes; a plain
/// fork() child gets ForkedProcessError from arena/registry calls.
///
/// Opening an arena and creating a variable can block on the arena's
/// cross-process lock, so those calls release the GIL (detach) while running.
/// Lookups, listing and value operations are lock-free and keep it.
#[pyclass(frozen, module = "atomvar", name = "Arena")]
pub struct PyArena(core::Arena);

#[pymethods]
impl PyArena {
    /// capacity=None adopts an existing arena's capacity (or creates one with the
    /// default); an explicit capacity must match an existing arena.
    #[new]
    #[pyo3(signature = (name, capacity = None))]
    fn new(py: Python<'_>, name: &str, capacity: Option<u32>) -> PyResult<Self> {
        py.detach(|| core::Arena::open(name, capacity)).map(Self).map_err(err)
    }

    #[getter]
    fn name(&self) -> String {
        self.0.name().to_string()
    }

    #[getter]
    fn capacity(&self) -> u32 {
        self.0.capacity()
    }

    /// Opens or creates an i64 variable; `init` applies only on creation.
    #[pyo3(signature = (name, init = 0))]
    fn int(&self, py: Python<'_>, name: &str, init: i64) -> PyResult<PyAtomicInt> {
        py.detach(|| self.0.i64(name, init)).map(PyAtomicInt).map_err(err)
    }
    #[pyo3(signature = (name, init = 0))]
    fn uint(&self, py: Python<'_>, name: &str, init: u64) -> PyResult<PyAtomicUInt> {
        py.detach(|| self.0.u64(name, init)).map(PyAtomicUInt).map_err(err)
    }
    #[pyo3(signature = (name, init = false))]
    fn bool(&self, py: Python<'_>, name: &str, init: bool) -> PyResult<PyAtomicBool> {
        py.detach(|| self.0.bool(name, init)).map(PyAtomicBool).map_err(err)
    }
    #[pyo3(signature = (name, init = 0.0))]
    fn float(&self, py: Python<'_>, name: &str, init: f64) -> PyResult<PyAtomicFloat> {
        py.detach(|| self.0.f64(name, init)).map(PyAtomicFloat).map_err(err)
    }
    #[pyo3(signature = (name, init = 0))]
    fn u128(&self, py: Python<'_>, name: &str, init: u128) -> PyResult<PyAtomicU128> {
        py.detach(|| self.0.u128(name, init)).map(PyAtomicU128).map_err(err)
    }

    /// Opens an existing variable (NotFoundError otherwise).
    fn open_int(&self, name: &str) -> PyResult<PyAtomicInt> {
        self.0.open_i64(name).map(PyAtomicInt).map_err(err)
    }
    fn open_uint(&self, name: &str) -> PyResult<PyAtomicUInt> {
        self.0.open_u64(name).map(PyAtomicUInt).map_err(err)
    }
    fn open_bool(&self, name: &str) -> PyResult<PyAtomicBool> {
        self.0.open_bool(name).map(PyAtomicBool).map_err(err)
    }
    fn open_float(&self, name: &str) -> PyResult<PyAtomicFloat> {
        self.0.open_f64(name).map(PyAtomicFloat).map_err(err)
    }
    fn open_u128(&self, name: &str) -> PyResult<PyAtomicU128> {
        self.0.open_u128(name).map(PyAtomicU128).map_err(err)
    }

    /// Type name ("i64", "u64", "bool", "f64", "u128") of a variable, or None.
    fn lookup(&self, name: &str) -> PyResult<Option<String>> {
        Ok(self
            .0
            .lookup(name)
            .map_err(err)?
            .map(|v| v.value_type.as_str().to_string()))
    }

    /// [(name, type_name), ...] for all published variables.
    fn list(&self) -> PyResult<Vec<(String, String)>> {
        Ok(self
            .0
            .list()
            .map_err(err)?
            .into_iter()
            .map(|v| (v.name, v.value_type.as_str().to_string()))
            .collect())
    }

    fn __len__(&self) -> PyResult<usize> {
        self.0.len().map_err(err)
    }

    fn __repr__(&self) -> String {
        format!("Arena({:?}, capacity={})", self.0.name(), self.0.capacity())
    }
}

/// Best-effort diagnostic (raw CPUID). Not a guarantee: see the README.
#[pyfunction]
fn cpu_supported() -> bool {
    core::cpu_supported()
}

#[pymodule(gil_used = false)]
fn _atomvar(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    m.add_class::<PyOrdering>()?;
    m.add_class::<PyAtomicInt>()?;
    m.add_class::<PyAtomicUInt>()?;
    m.add_class::<PyAtomicBool>()?;
    m.add_class::<PyAtomicFloat>()?;
    m.add_class::<PyAtomicU128>()?;
    m.add_class::<PyArena>()?;
    m.add_function(wrap_pyfunction!(cpu_supported, m)?)?;
    m.add("AtomvarError", py.get_type::<AtomvarError>())?;
    m.add("InvalidOrderingError", py.get_type::<InvalidOrderingError>())?;
    m.add("InvalidNameError", py.get_type::<InvalidNameError>())?;
    m.add("ArenaFullError", py.get_type::<ArenaFullError>())?;
    m.add("TypeMismatchError", py.get_type::<TypeMismatchError>())?;
    m.add("NotFoundError", py.get_type::<NotFoundError>())?;
    m.add("LayoutMismatchError", py.get_type::<LayoutMismatchError>())?;
    m.add("ForkedProcessError", py.get_type::<ForkedProcessError>())?;
    m.add("UnsupportedCpuError", py.get_type::<UnsupportedCpuError>())?;
    m.add("DEFAULT_CAPACITY", core::DEFAULT_CAPACITY)?;
    Ok(())
}
