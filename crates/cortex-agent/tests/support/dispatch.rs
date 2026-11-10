//! 旧协议测试的显式 mock。只与 FakeComputerDriver 配合，不证明持久化。
//! 真实 SQLite/取消/跨进程验收位于 cortex-local::computer_ledger::tests。
use cortex_agent::computer_dispatch::{
    AuthorizedDispatch, DispatchOutcome, DispatchPermit, DurableDispatch,
};
use cortex_agent::computer_driver::{ComputerDriver, ComputerGateway};
use cortex_agent::computer_policy::ComputerPolicy;
use std::sync::Arc;

#[derive(Debug)]
struct ProtocolDispatch;
struct ProtocolPermit;

#[async_trait::async_trait]
impl DurableDispatch for ProtocolDispatch {
    async fn prepare(&self, _: AuthorizedDispatch) -> Result<Box<dyn DispatchPermit>, String> {
        Ok(Box::new(ProtocolPermit))
    }
}
#[async_trait::async_trait]
impl DispatchPermit for ProtocolPermit {
    async fn finish(self: Box<Self>, _: DispatchOutcome) -> Result<(), String> {
        Ok(())
    }
}
pub fn gateway<D: ComputerDriver>(policy: ComputerPolicy, driver: D) -> ComputerGateway<D> {
    ComputerGateway::new(policy, driver).with_dispatch(Arc::new(ProtocolDispatch))
}
