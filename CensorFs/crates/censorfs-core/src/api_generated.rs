// This file is checked in so target builds do not require protoc.
// Source: api/censorfs.proto. Regenerate with prost-build when the schema changes.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct RequestContext {
    #[prost(uint32, tag = "1")]
    pub protocol_version: u32,
    #[prost(bytes = "vec", tag = "2")]
    pub request_id: ::prost::alloc::vec::Vec<u8>,
    #[prost(string, tag = "3")]
    pub actor_id: ::prost::alloc::string::String,
    #[prost(uint64, tag = "4")]
    pub mount_epoch: u64,
    #[prost(uint64, tag = "5")]
    pub deadline_unix_ms: u64,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ControlRequest {
    #[prost(message, optional, tag = "1")]
    pub context: ::core::option::Option<RequestContext>,
    #[prost(enumeration = "Operation", tag = "2")]
    pub operation: i32,
    #[prost(bytes = "vec", tag = "3")]
    pub payload: ::prost::alloc::vec::Vec<u8>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ControlResponse {
    #[prost(bytes = "vec", tag = "1")]
    pub request_id: ::prost::alloc::vec::Vec<u8>,
    #[prost(uint32, tag = "2")]
    pub status: u32,
    #[prost(string, tag = "3")]
    pub message: ::prost::alloc::string::String,
    #[prost(bytes = "vec", tag = "4")]
    pub payload: ::prost::alloc::vec::Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, ::prost::Enumeration)]
#[repr(i32)]
pub enum Operation {
    Unspecified = 0,
    GetFsInfo = 1,
    GetRequestResult = 2,
    InspectRecovery = 3,
    BeginTx = 10,
    CloseTx = 11,
    AbortTx = 12,
    OpenView = 20,
    CloseView = 21,
    CreateBranch = 30,
    GetBranchHead = 31,
    ListBranches = 32,
    CreateBranchFrom = 33,
    BeginTicket = 40,
    PrepareTicket = 41,
    AbortTicket = 42,
    Explore = 43,
    AbortExploration = 44,
    Publish = 50,
    BuildRollbackCandidate = 51,
    CommitExploration = 52,
    GetGenerationInfo = 60,
    ResolveForkPoint = 61,
    DiffGenerations = 62,
    MergeBranches = 63,
    DiffText = 64,
    TestList = 70,
    TestCat = 71,
    TestWrite = 72,
    TestMkdir = 73,
    TestRemove = 74,
    TestMove = 75,
    VariantOpen = 80,
    VariantPrepare = 81,
    VariantPublish = 82,
    VariantAbort = 83,
    AttachFuse = 90,
}
