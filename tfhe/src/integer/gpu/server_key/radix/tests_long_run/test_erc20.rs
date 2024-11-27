use crate::integer::gpu::server_key::radix::tests_unsigned::{
    create_gpu_parameterized_test, GpuFunctionExecutor,
};
use crate::integer::gpu::CudaServerKey;
use crate::integer::server_key::radix_parallel::tests_long_run::test_erc20::{
    no_cmux_erc20_test, safe_erc20_test, whitepaper_erc20_test,
};
use crate::shortint::parameters::*;

create_gpu_parameterized_test!(safe_erc20 {
    PARAM_GPU_MULTI_BIT_GROUP_3_MESSAGE_2_CARRY_2_KS_PBS_TUNIFORM_2M64
});
create_gpu_parameterized_test!(whitepaper_erc20 {
    PARAM_GPU_MULTI_BIT_GROUP_3_MESSAGE_2_CARRY_2_KS_PBS_TUNIFORM_2M64
});
create_gpu_parameterized_test!(no_cmux_erc20 {
    PARAM_GPU_MULTI_BIT_GROUP_3_MESSAGE_2_CARRY_2_KS_PBS_TUNIFORM_2M64
});

fn safe_erc20<P>(param: P)
where
    P: Into<PBSParameters>,
{
    let overflowing_add_executor =
        GpuFunctionExecutor::new(&CudaServerKey::unsigned_overflowing_add);
    let overflowing_sub_executor =
        GpuFunctionExecutor::new(&CudaServerKey::unsigned_overflowing_sub);
    let if_then_else_executor = GpuFunctionExecutor::new(&CudaServerKey::if_then_else);
    let bitwise_or_executor = GpuFunctionExecutor::new(&CudaServerKey::bitor);
    safe_erc20_test(
        param,
        overflowing_add_executor,
        overflowing_sub_executor,
        if_then_else_executor,
        bitwise_or_executor,
    );
}

fn whitepaper_erc20<P>(param: P)
where
    P: Into<PBSParameters>,
{
    let ge_executor = GpuFunctionExecutor::new(&CudaServerKey::ge);
    let add_executor = GpuFunctionExecutor::new(&CudaServerKey::add);
    let if_then_else_executor = GpuFunctionExecutor::new(&CudaServerKey::if_then_else);
    let sub_executor = GpuFunctionExecutor::new(&CudaServerKey::sub);
    whitepaper_erc20_test(
        param,
        ge_executor,
        add_executor,
        if_then_else_executor,
        sub_executor,
    );
}

fn no_cmux_erc20<P>(param: P)
where
    P: Into<PBSParameters>,
{
    let ge_executor = GpuFunctionExecutor::new(&CudaServerKey::ge);
    let mul_executor = GpuFunctionExecutor::new(&CudaServerKey::mul);
    let add_executor = GpuFunctionExecutor::new(&CudaServerKey::add);
    let sub_executor = GpuFunctionExecutor::new(&CudaServerKey::sub);
    no_cmux_erc20_test(param, ge_executor, mul_executor, add_executor, sub_executor);
}