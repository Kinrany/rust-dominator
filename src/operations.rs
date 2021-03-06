use std::rc::Rc;
use std::cell::RefCell;
use std::future::Future;
use std::iter::IntoIterator;

use discard::{Discard, DiscardOnDrop};
use futures_util::future::ready;
use futures_signals::{cancelable_future, CancelableFutureHandle};
use futures_signals::signal::{Signal, SignalExt};
use futures_signals::signal_vec::{VecDiff, SignalVec, SignalVecExt};
use web_sys::Node;
use wasm_bindgen::UnwrapThrowExt;
use wasm_bindgen_futures::spawn_local;

use crate::bindings;
use crate::dom::Dom;
use crate::callbacks::Callbacks;


#[inline]
pub(crate) fn spawn_future<F>(future: F) -> DiscardOnDrop<CancelableFutureHandle>
    where F: Future<Output = ()> + 'static {
    // TODO make this more efficient ?
    let (handle, future) = cancelable_future(future, || ());

    spawn_local(future);

    handle
}


#[inline]
pub(crate) fn for_each<A, B>(signal: A, mut callback: B) -> CancelableFutureHandle
    where A: Signal + 'static,
          B: FnMut(A::Item) + 'static {

    DiscardOnDrop::leak(spawn_future(signal.for_each(move |value| {
        callback(value);
        ready(())
    })))
}


#[inline]
fn for_each_vec<A, B>(signal: A, mut callback: B) -> CancelableFutureHandle
    where A: SignalVec + 'static,
          B: FnMut(VecDiff<A::Item>) + 'static {

    DiscardOnDrop::leak(spawn_future(signal.for_each(move |value| {
        callback(value);
        ready(())
    })))
}


#[inline]
pub(crate) fn insert_children_iter<A: std::borrow::BorrowMut<Dom>, B: IntoIterator<Item = A>>(element: &Node, callbacks: &mut Callbacks, value: B) {
    fn insert_children_one(element: &Node, callbacks: &mut Callbacks, dom: &mut Dom) {
        // TODO can this be made more efficient ?
        callbacks.after_insert.append(&mut dom.callbacks.after_insert);
        callbacks.after_remove.append(&mut dom.callbacks.after_remove);

        bindings::append_child(element, &dom.element);
    }

    for mut dom in value {
        let dom = std::borrow::BorrowMut::borrow_mut(&mut dom);
        insert_children_one(element, callbacks, dom);
    }
}


fn after_insert(is_inserted: bool, callbacks: &mut Callbacks) {
    callbacks.leak();

    if is_inserted {
        callbacks.trigger_after_insert();
    }
}


#[inline]
pub(crate) fn insert_child_signal<A>(element: Node, callbacks: &mut Callbacks, signal: A)
    where A: Signal<Item = Option<Dom>> + 'static {

    struct State {
        is_inserted: bool,
        child: Option<Dom>,
    }

    impl State {
        fn new() -> Rc<RefCell<Self>> {
            Rc::new(RefCell::new(State {
                is_inserted: false,
                child: None,
            }))
        }

        fn after_insert(state: Rc<RefCell<Self>>, callbacks: &mut Callbacks) {
            callbacks.after_insert(move |_| {
                let mut state = state.borrow_mut();

                if !state.is_inserted {
                    state.is_inserted = true;

                    if let Some(ref mut child) = state.child {
                        child.callbacks.trigger_after_insert();
                    }
                }
            });
        }

        // TODO verify that this will drop `child`
        fn after_remove(&mut self, element: &Node, mut child: Option<Dom>) {
            if let Some(old_child) = self.child.take() {
                bindings::remove_child(&element, &old_child.element);

                old_child.callbacks.discard();
            }

            if let Some(ref mut new_child) = child {
                bindings::append_child(&element, &new_child.element);

                after_insert(self.is_inserted, &mut new_child.callbacks);
            }

            self.child = child;
        }
    }

    let state = State::new();

    State::after_insert(state.clone(), callbacks);

    callbacks.after_remove(for_each(signal, move |child| {
        let mut state = state.borrow_mut();
        state.after_remove(&element, child);
    }));
}


#[inline]
pub(crate) fn insert_children_signal_vec<A>(element: Node, callbacks: &mut Callbacks, signal: A)
    where A: SignalVec<Item = Dom> + 'static {

    struct State {
        is_inserted: bool,
        children: Vec<Dom>,
    }

    impl State {
        fn new() -> Rc<RefCell<Self>> {
            Rc::new(RefCell::new(State {
                is_inserted: false,
                children: vec![],
            }))
        }

        fn after_insert(state: Rc<RefCell<Self>>, callbacks: &mut Callbacks) {
            callbacks.after_insert(move |_| {
                let mut state = state.borrow_mut();

                if !state.is_inserted {
                    state.is_inserted = true;

                    for dom in state.children.iter_mut() {
                        dom.callbacks.trigger_after_insert();
                    }
                }
            });
        }

        fn clear(&mut self, element: &Node) {
            // TODO is this correct ?
            if self.children.len() > 0 {
                bindings::remove_all_children(element);

                for dom in self.children.drain(..) {
                    dom.callbacks.discard();
                }
            }
        }

        fn insert_at(&self, element: &Node, new_index: usize, child: &Node) {
            if let Some(dom) = self.children.get(new_index) {
                bindings::insert_child_before(element, child, &dom.element);

            } else {
                bindings::append_child(element, child);
            }
        }

        // TODO verify that this will drop `children`
        fn process_change(&mut self, element: &Node, change: VecDiff<Dom>) {
            match change {
                VecDiff::Replace { values } => {
                    self.clear(element);

                    self.children = values;

                    let is_inserted = self.is_inserted;

                    for dom in self.children.iter_mut() {
                        bindings::append_child(element, &dom.element);

                        after_insert(is_inserted, &mut dom.callbacks);
                    }
                },

                VecDiff::InsertAt { index, mut value } => {
                    self.insert_at(element, index, &value.element);

                    after_insert(self.is_inserted, &mut value.callbacks);

                    // TODO figure out a way to move this to the top
                    self.children.insert(index, value);
                },

                VecDiff::Push { mut value } => {
                    bindings::append_child(element, &value.element);

                    after_insert(self.is_inserted, &mut value.callbacks);

                    // TODO figure out a way to move this to the top
                    self.children.push(value);
                },

                VecDiff::UpdateAt { index, mut value } => {
                    let dom = &mut self.children[index];

                    bindings::replace_child(element, &value.element, &dom.element);

                    after_insert(self.is_inserted, &mut value.callbacks);

                    // TODO figure out a way to move this to the top
                    // TODO test this
                    ::std::mem::swap(dom, &mut value);

                    value.callbacks.discard();
                },

                VecDiff::Move { old_index, new_index } => {
                    let value = self.children.remove(old_index);

                    self.insert_at(element, new_index, &value.element);

                    self.children.insert(new_index, value);
                },

                VecDiff::RemoveAt { index } => {
                    let dom = self.children.remove(index);

                    bindings::remove_child(element, &dom.element);

                    dom.callbacks.discard();
                },

                VecDiff::Pop {} => {
                    let dom = self.children.pop().unwrap_throw();

                    bindings::remove_child(element, &dom.element);

                    dom.callbacks.discard();
                },

                VecDiff::Clear {} => {
                    self.clear(element);
                },
            }
        }
    }

    let state = State::new();

    State::after_insert(state.clone(), callbacks);

    callbacks.after_remove(for_each_vec(signal, move |change| {
        let mut state = state.borrow_mut();
        state.process_change(&element, change);
    }));
}
