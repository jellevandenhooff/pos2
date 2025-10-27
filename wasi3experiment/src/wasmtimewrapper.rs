use futures::stream::{FuturesUnordered, StreamExt};
use std::pin::Pin;
use tokio::sync::oneshot::error::RecvError;
use tokio_util::sync::CancellationToken;
use wasmtime::component::Accessor;
use wasmtime::{Result, Store};

type WorkItem<State, Instance> = Box<
    dyn for<'a> FnOnce(
            &'a Accessor<State>,
            &'a Instance,
        ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>
        + Send,
>;

use tokio::sync::mpsc;

pub struct WorkSender<State: 'static, Instance> {
    send: Option<mpsc::Sender<WorkItem<State, Instance>>>,
}

pub struct WorkWorker<State: 'static, Instance> {
    recv: mpsc::Receiver<WorkItem<State, Instance>>,
    stop: CancellationToken,
    store: Store<State>,
    instance: Instance,
}

pub trait WorkFn<A, B, O>: Send + FnOnce(A, B) -> Self::Fut {
    /// The produced subsystem future
    type Fut: Future<Output = O> + Send;
}

impl<A, B, O, Out, F> WorkFn<A, B, O> for F
where
    Out: Future<Output = O> + Send,
    F: Send + FnOnce(A, B) -> Out,
{
    type Fut = Out;
}

impl<State: Send, Instance: Send + Sync> WorkSender<State, Instance> {
    pub async fn submit<F, O>(&self, f: F) -> impl Future<Output = Result<O, RecvError>> + 'static
    where
        F: 'static + Send + for<'a> WorkFn<&'a Accessor<State>, &'a Instance, O>,
        // TODO: remove Debug, artifact of unwrap() I think
        O: std::fmt::Debug + Send + 'static,
    {
        let (send, recv) = tokio::sync::oneshot::channel();

        if let Some(x) = &self.send {
            x.send(Box::new(|accessor, state| {
                Box::pin(async move {
                    send.send(f(accessor, state).await).unwrap();
                })
            }))
            .await
            .unwrap();
        }

        recv
    }
}

impl<State: Send, Instance> WorkWorker<State, Instance> {
    pub async fn run(self) {
        let mut store = self.store;
        let stop = self.stop.clone();
        let instance = self.instance;
        let mut recv = self.recv;

        store
            .run_concurrent(async move |store| -> Result<_> {
                let mut running_tasks = FuturesUnordered::new();

                loop {
                    tokio::select! {
                        _ = stop.cancelled() => {
                            break;
                        }
                        task = recv.recv() => {
                            let Some(task) = task else {
                                break;
                            };

                            let fut = task(store, &instance);
                            running_tasks.push(fut);
                        }
                        _ = running_tasks.next(), if !running_tasks.is_empty() => {
                            // cool
                        }
                    }
                }

                // wait for tasks to stop
                while let Some(_) = running_tasks.next().await {}

                Ok(())
            })
            .await
            .unwrap()
            .unwrap();
    }
}

pub fn make_worker<State, Instance>(
    store: Store<State>,
    instance: Instance,
) -> (WorkSender<State, Instance>, WorkWorker<State, Instance>) {
    let stop = CancellationToken::new();
    let (send, recv) = tokio::sync::mpsc::channel::<WorkItem<State, Instance>>(1024);
    let sender = WorkSender { send: Some(send) };
    let worker = WorkWorker {
        recv: recv,
        stop: stop,
        store: store,
        instance: instance,
    };
    (sender, worker)
}
