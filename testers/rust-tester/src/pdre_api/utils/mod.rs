mod child_storage;
mod crypto;
mod network;
mod storage;

pub use child_storage::ChildStorageApi;
pub use crypto::CryptoApi;
pub use network::NetworkApi;
pub use storage::StorageApi;

use substrate_executor::error::Error;
use substrate_executor::WasmExecutor;
use substrate_primitives::testing::KeyStore;
use substrate_primitives::Blake2Hasher;
use substrate_state_machine::TestExternalities as CoreTestExternalities;
use wasmi::MemoryRef;
use wasmi::RuntimeValue::{self, I32};

use std::cell::RefCell;
use std::rc::Rc;

type TestExternalities<H> = CoreTestExternalities<H, u64>;

// Convenience function:
// Gets the Wasm blob which was generated by the `build.rs` script
fn get_wasm_blob() -> Vec<u8> {
    use std::fs::File;
    use std::io::prelude::*;

    let mut f =
        File::open("test/testers/rust-tester/target/wasm32-unknown-unknown/release/wasm_blob.wasm")
            .expect("Failed to open wasm blob in target");
    let mut buffer = Vec::new();
    f.read_to_end(&mut buffer)
        .expect("Failed to load wasm blob into memory");
    buffer
}

fn le(num: &mut u32) -> [u8; 4] {
    num.to_le_bytes()
}

fn wrap<T>(t: T) -> Rc<RefCell<T>> {
    Rc::new(RefCell::new(t))
}

fn copy_slice(scoped: Rc<RefCell<Vec<u8>>>, output: &mut [u8]) {
    output.copy_from_slice(scoped.borrow().as_slice());
}

fn copy_u32(scope: Rc<RefCell<u32>>, num: &mut u32) {
    *num = *scope.borrow();
}

struct CallWasm<'a> {
    ext: &'a mut TestExternalities<Blake2Hasher>,
    blob: &'a [u8],
    method: &'a str,
    //create_param: Box<FnOnce(&mut dyn FnMut(&[u8]) -> Result<u32, Error>) -> Result<Vec<RuntimeValue>, Error>>,
}

impl<'a> CallWasm<'a> {
    fn new(ext: &'a mut TestExternalities<Blake2Hasher>, blob: &'a [u8], method: &'a str) -> Self {
        CallWasm {
            ext: ext,
            blob: blob,
            method: method,
        }
    }
    /// Calls the final Wasm Runtime function (this method does not get used directly)
    fn call<F, FR, R>(&mut self, create_param: F, filter_return: FR) -> Result<R, Error>
    where
        F: FnOnce(&mut dyn FnMut(&[u8]) -> Result<u32, Error>) -> Result<Vec<RuntimeValue>, Error>,
        FR: FnOnce(Option<RuntimeValue>, &MemoryRef) -> Result<Option<R>, Error>,
    {
        WasmExecutor::new().call_with_custom_signature(
            self.ext,
            1,
            self.blob,
            self.method,
            create_param,
            filter_return,
        )
    }
    /// Generate the parameters according to `data`. `len_index` refers to the index in `data`
    /// of which the parameter lenght must be included.
    fn gen_params(
        data: &[&[u8]],
        len_index: &[usize],
        ptr: Option<Rc<RefCell<u32>>>,
    ) -> impl FnOnce(&mut dyn FnMut(&[u8]) -> Result<u32, Error>) -> Result<Vec<RuntimeValue>, Error>
    {
        let data_c: Vec<Vec<u8>> = data.iter().map(|d| d.to_vec()).collect();
        let len_index_c = len_index.to_owned();

        move |alloc| {
            let mut offsets = vec![];
            for d in &data_c {
                offsets.push(alloc(d)?);
            }

            // If a pointer was passed, assign address of the last parameter (the last parameter holds the output)
            if ptr.is_some() && offsets.len() >= 1 {
                *ptr.as_ref().unwrap().borrow_mut() = **offsets.last().as_ref().unwrap() as u32;
            }

            let mut counter = 0;
            let mut runtime_vals = vec![];
            for off in offsets {
                // Push the offset to vals
                runtime_vals.push(I32(off as i32));
                // If there also must be the length, push too
                if len_index_c.contains(&counter) {
                    runtime_vals.push(I32(data_c[counter].len() as i32))
                }
                counter += 1;
            }

            Ok(runtime_vals)
        }
    }
    fn return_none(
    ) -> impl FnOnce(Option<RuntimeValue>, &MemoryRef) -> Result<Option<()>, Error> {
        |_, _| { Ok(Some(()))}
    }
    fn return_none_write_buffer(
        output: Rc<RefCell<Vec<u8>>>,
        ptr: Rc<RefCell<u32>>,
    ) -> impl FnOnce(Option<RuntimeValue>, &MemoryRef) -> Result<Option<()>, Error> {
        move |_, memory| {
            let mut output_b = output.borrow_mut();
            let len = output_b.len();

            output_b.copy_from_slice(
                memory
                    .get(*ptr.borrow(), len)
                    .map_err(|_| Error::Runtime)?
                    .as_slice(),
            );
            Ok(Some(()))
        }
    }
    fn return_value_no_buffer(
    ) -> impl FnOnce(Option<RuntimeValue>, &MemoryRef) -> Result<Option<u32>, Error> {
        |res, _| {
            if let Some(I32(r)) = res {
                Ok(Some(r as u32))
            } else {
                Ok(None)
            }
        }
    }
    fn return_value_write_buffer(
        output: Rc<RefCell<Vec<u8>>>,
        ptr: Rc<RefCell<u32>>,
    ) -> impl FnOnce(Option<RuntimeValue>, &MemoryRef) -> Result<Option<u32>, Error> {
        move |res, memory| {
            let mut output_b = output.borrow_mut();
            let len = output_b.len();

            if let Some(I32(r)) = res {
                output_b.copy_from_slice(
                    memory
                        .get(*ptr.borrow(), len)
                        .map_err(|_| Error::Runtime)?
                        .as_slice(),
                );

                Ok(Some(r as u32))
            } else {
                Ok(None)
            }
        }
    }
    fn return_buffer(
        result_len: Rc<RefCell<u32>>,
        ptr: Rc<RefCell<u32>>,
    ) -> impl FnOnce(Option<RuntimeValue>, &MemoryRef) -> Result<Option<Vec<u8>>, Error> {
        move |res, memory| {
            let mut result_len_b = result_len.borrow_mut();
            use std::convert::TryInto;
            if let Some(I32(r)) = res {
                *result_len_b = u32::from_le_bytes(
                    memory.get(*ptr.borrow(), 4).unwrap().as_slice()[0..4]
                        .try_into()
                        .unwrap(),
                );

                if r == 0 {
                    return Ok(Some(vec![]));
                }

                memory
                    .get(r as u32, *result_len_b as usize)
                    .map_err(|_| Error::Runtime)
                    .map(Some)
            } else {
                Ok(None)
            }
        }
    }
}
