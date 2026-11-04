use super::*;

struct IdleActor {
    _resource: Arc<()>,
}

impl PackTaskActor for IdleActor {
    type Request = ();
    type Operation = ();
    type Pull = ();
    type Error = ();

    fn is_available(&self) -> bool {
        true
    }

    fn is_fatal(_error: Self::Error) -> bool {
        false
    }

    async fn invoke(&mut self, _request: Self::Request) -> Result<Self::Operation, Self::Error> {
        unreachable!("idle actor receives no operations")
    }

    async fn pull(&mut self, _operation: &mut Self::Operation) -> Result<Self::Pull, Self::Error> {
        unreachable!("idle actor receives no operations")
    }

    async fn drop_operation(&mut self, _operation: Self::Operation) -> Result<(), Self::Error> {
        Ok(())
    }

    fn take_admission(_operation: &mut Self::Operation) -> Option<TaskOperationAdmission> {
        None
    }
}

#[tokio::test]
async fn shutdown_waiters_observe_actor_resources_released_before_returning() {
    for started in [false, true] {
        let resource = Arc::new(());
        let released = Arc::downgrade(&resource);
        let client = TaskActorClient::start(IdleActor {
            _resource: resource,
        });
        if started {
            tokio::task::yield_now().await;
        }
        client.shutdown();
        assert!(
            released.upgrade().is_some(),
            "an abort request alone does not release the actor"
        );
        tokio::join!(client.wait_stopped(), client.wait_stopped());
        assert!(
            released.upgrade().is_none(),
            "all shutdown waiters must observe resource teardown"
        );
        client.wait_stopped().await;
    }
}
