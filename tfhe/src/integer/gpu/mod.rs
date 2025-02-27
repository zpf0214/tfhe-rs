pub mod ciphertext;
pub mod client_key;
pub mod list_compression;
pub mod server_key;

use crate::core_crypto::gpu::slice::{CudaSlice, CudaSliceMut};
use crate::core_crypto::gpu::vec::CudaVec;
use crate::core_crypto::gpu::CudaStreams;
use crate::core_crypto::prelude::{
    DecompositionBaseLog, DecompositionLevelCount, GlweDimension, LweBskGroupingFactor,
    LweDimension, Numeric, PolynomialSize, UnsignedInteger,
};
use crate::integer::{ClientKey, RadixClientKey};
use crate::shortint::{CarryModulus, MessageModulus};
pub use server_key::CudaServerKey;
use std::cmp::min;

use crate::integer::server_key::radix_parallel::OutputFlag;
use tfhe_cuda_backend::bindings::*;
use tfhe_cuda_backend::cuda_bind::*;

#[repr(u32)]
#[derive(Clone, Copy)]
pub enum BitOpType {
    And = 0,
    Or = 1,
    Xor = 2,
    ScalarAnd = 3,
    ScalarOr = 4,
    ScalarXor = 5,
}

#[allow(dead_code)]
#[repr(u32)]
pub enum PBSType {
    MultiBit = 0,
    Classical = 1,
}

#[repr(u32)]
pub enum ShiftRotateType {
    LeftShift = 0,
    RightShift = 1,
    LeftRotate = 2,
    RightRotate = 3,
}

#[repr(u32)]
pub enum ComparisonType {
    EQ = 0,
    NE = 1,
    GT = 2,
    GE = 3,
    LT = 4,
    LE = 5,
    MAX = 6,
    MIN = 7,
}

pub fn gen_keys_gpu<P>(parameters_set: P, streams: &CudaStreams) -> (ClientKey, CudaServerKey)
where
    P: TryInto<crate::shortint::parameters::ShortintParameterSet>,
    <P as TryInto<crate::shortint::parameters::ShortintParameterSet>>::Error: std::fmt::Debug,
{
    let shortint_parameters_set: crate::shortint::parameters::ShortintParameterSet =
        parameters_set.try_into().unwrap();

    let is_wopbs_only_params = shortint_parameters_set.wopbs_only();

    // TODO
    // Manually manage the wopbs only case as a workaround pending wopbs rework
    // WOPBS used for PBS have no known failure probability at the moment, putting 1.0 for now
    let shortint_parameters_set = if is_wopbs_only_params {
        let wopbs_params = shortint_parameters_set.wopbs_parameters().unwrap();
        let pbs_params = crate::shortint::parameters::ClassicPBSParameters {
            lwe_dimension: wopbs_params.lwe_dimension,
            glwe_dimension: wopbs_params.glwe_dimension,
            polynomial_size: wopbs_params.polynomial_size,
            lwe_noise_distribution: wopbs_params.lwe_noise_distribution,
            glwe_noise_distribution: wopbs_params.glwe_noise_distribution,
            pbs_base_log: wopbs_params.pbs_base_log,
            pbs_level: wopbs_params.pbs_level,
            ks_base_log: wopbs_params.ks_base_log,
            ks_level: wopbs_params.ks_level,
            message_modulus: wopbs_params.message_modulus,
            carry_modulus: wopbs_params.carry_modulus,
            max_noise_level: crate::shortint::parameters::MaxNoiseLevel::from_msg_carry_modulus(
                wopbs_params.message_modulus,
                wopbs_params.carry_modulus,
            ),
            log2_p_fail: 1.0,
            ciphertext_modulus: wopbs_params.ciphertext_modulus,
            encryption_key_choice: wopbs_params.encryption_key_choice,
        };

        crate::shortint::parameters::ShortintParameterSet::try_new_pbs_and_wopbs_param_set((
            pbs_params,
            wopbs_params,
        ))
        .unwrap()
    } else {
        shortint_parameters_set
    };

    let gen_keys_inner = |parameters_set, streams: &CudaStreams| {
        let cks = ClientKey::new(parameters_set);
        let sks = CudaServerKey::new(&cks, streams);

        (cks, sks)
    };

    // #[cfg(any(test, feature = "internal-keycache"))]
    // {
    //     if is_wopbs_only_params {
    //         // TODO
    //         // Keycache is broken for the wopbs only case, so generate keys instead
    //         gen_keys_inner(shortint_parameters_set)
    //     } else {
    //         keycache::KEY_CACHE.get_from_params(shortint_parameters_set.pbs_parameters().
    // unwrap())     }
    // }
    // #[cfg(all(not(test), not(feature = "internal-keycache")))]
    // {
    gen_keys_inner(shortint_parameters_set, streams)
    // }
}

/// Generate a couple of client and server keys with given parameters
///
/// Contrary to [gen_keys_gpu], this returns a [RadixClientKey]
///
/// ```rust
/// use tfhe::core_crypto::gpu::CudaStreams;
/// use tfhe::core_crypto::gpu::vec::GpuIndex;
/// use tfhe::integer::gpu::gen_keys_radix_gpu;
/// use tfhe::shortint::parameters::PARAM_GPU_MULTI_BIT_GROUP_3_MESSAGE_2_CARRY_2_KS_PBS_TUNIFORM_2M64;
///
/// let gpu_index = 0;
/// let mut streams = CudaStreams::new_single_gpu(GpuIndex(gpu_index));
/// // generate the client key and the server key:
/// let num_blocks = 4;
/// let (cks, sks) = gen_keys_radix_gpu(PARAM_GPU_MULTI_BIT_GROUP_3_MESSAGE_2_CARRY_2_KS_PBS_TUNIFORM_2M64, num_blocks, &mut streams);
/// ```
pub fn gen_keys_radix_gpu<P>(
    parameters_set: P,
    num_blocks: usize,
    streams: &CudaStreams,
) -> (RadixClientKey, CudaServerKey)
where
    P: TryInto<crate::shortint::parameters::ShortintParameterSet>,
    <P as TryInto<crate::shortint::parameters::ShortintParameterSet>>::Error: std::fmt::Debug,
{
    let (cks, sks) = gen_keys_gpu(parameters_set, streams);

    (RadixClientKey::from((cks, num_blocks)), sks)
}

#[allow(clippy::too_many_arguments)]
/// # Safety
///
/// - [CudaStreams::synchronize] __must__ be called after this function as soon as synchronization
///   is required
pub unsafe fn scalar_addition_integer_radix_assign_async<T: UnsignedInteger>(
    streams: &CudaStreams,
    lwe_array: &mut CudaVec<T>,
    scalar_input: &CudaVec<T>,
    lwe_dimension: LweDimension,
    num_samples: u32,
    message_modulus: u32,
    carry_modulus: u32,
) {
    assert_eq!(
        streams.gpu_indexes[0],
        lwe_array.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        scalar_input.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    cuda_scalar_addition_integer_radix_ciphertext_64_inplace(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        lwe_array.as_mut_c_ptr(0),
        scalar_input.as_c_ptr(0),
        lwe_dimension.0 as u32,
        num_samples,
        message_modulus,
        carry_modulus,
    );
}

#[allow(clippy::too_many_arguments)]
/// # Safety
///
/// - [CudaStreams::synchronize] __must__ be called after this function as soon as synchronization
///   is required
pub unsafe fn unchecked_scalar_mul_integer_radix_kb_async<T: UnsignedInteger, B: Numeric>(
    streams: &CudaStreams,
    lwe_array: &mut CudaVec<T>,
    decomposed_scalar: &[T],
    has_at_least_one_set: &[T],
    bootstrapping_key: &CudaVec<B>,
    keyswitch_key: &CudaVec<u64>,
    message_modulus: MessageModulus,
    carry_modulus: CarryModulus,
    glwe_dimension: GlweDimension,
    polynomial_size: PolynomialSize,
    lwe_dimension: LweDimension,
    pbs_base_log: DecompositionBaseLog,
    pbs_level: DecompositionLevelCount,
    ks_base_log: DecompositionBaseLog,
    ks_level: DecompositionLevelCount,
    num_blocks: u32,
    num_scalars: u32,
    pbs_type: PBSType,
    grouping_factor: LweBskGroupingFactor,
) {
    assert_eq!(
        streams.gpu_indexes[0],
        lwe_array.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        bootstrapping_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        keyswitch_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    let mut mem_ptr: *mut i8 = std::ptr::null_mut();
    scratch_cuda_integer_scalar_mul_kb_64(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
        glwe_dimension.0 as u32,
        polynomial_size.0 as u32,
        lwe_dimension.0 as u32,
        ks_level.0 as u32,
        ks_base_log.0 as u32,
        pbs_level.0 as u32,
        pbs_base_log.0 as u32,
        grouping_factor.0 as u32,
        num_blocks,
        message_modulus.0 as u32,
        carry_modulus.0 as u32,
        pbs_type as u32,
        true,
    );

    cuda_scalar_multiplication_integer_radix_ciphertext_64_inplace(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        lwe_array.as_mut_c_ptr(0),
        decomposed_scalar.as_ptr().cast::<u64>(),
        has_at_least_one_set.as_ptr().cast::<u64>(),
        mem_ptr,
        bootstrapping_key.ptr.as_ptr(),
        keyswitch_key.ptr.as_ptr(),
        (glwe_dimension.0 * polynomial_size.0) as u32,
        polynomial_size.0 as u32,
        message_modulus.0 as u32,
        num_blocks,
        num_scalars,
    );

    cleanup_cuda_integer_radix_scalar_mul(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
    );
}

#[allow(clippy::too_many_arguments)]
/// # Safety
///
/// - [CudaStreams::synchronize] __must__ be called after this function as soon as synchronization
///   is required
pub unsafe fn compress_integer_radix_async<T: UnsignedInteger>(
    streams: &CudaStreams,
    glwe_array_out: &mut CudaVec<T>,
    lwe_array_in: &CudaVec<T>,
    fp_keyswitch_key: &CudaVec<u64>,
    message_modulus: MessageModulus,
    carry_modulus: CarryModulus,
    compression_glwe_dimension: GlweDimension,
    compression_polynomial_size: PolynomialSize,
    lwe_dimension: LweDimension,
    ks_base_log: DecompositionBaseLog,
    ks_level: DecompositionLevelCount,
    lwe_per_glwe: u32,
    storage_log_modulus: u32,
    num_blocks: u32,
) {
    assert_eq!(
        streams.gpu_indexes[0],
        glwe_array_out.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        lwe_array_in.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        fp_keyswitch_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    let mut mem_ptr: *mut i8 = std::ptr::null_mut();
    scratch_cuda_integer_compress_radix_ciphertext_64(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
        compression_glwe_dimension.0 as u32,
        compression_polynomial_size.0 as u32,
        lwe_dimension.0 as u32,
        ks_level.0 as u32,
        ks_base_log.0 as u32,
        num_blocks,
        message_modulus.0 as u32,
        carry_modulus.0 as u32,
        PBSType::Classical as u32,
        lwe_per_glwe,
        storage_log_modulus,
        true,
    );

    cuda_integer_compress_radix_ciphertext_64(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        glwe_array_out.as_mut_c_ptr(0),
        lwe_array_in.as_c_ptr(0),
        fp_keyswitch_key.ptr.as_ptr(),
        num_blocks,
        mem_ptr,
    );

    cleanup_cuda_integer_compress_radix_ciphertext_64(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
    );
}

#[allow(clippy::too_many_arguments)]
/// # Safety
///
/// - [CudaStreams::synchronize] __must__ be called after this function as soon as synchronization
///   is required
pub unsafe fn decompress_integer_radix_async<T: UnsignedInteger, B: Numeric>(
    streams: &CudaStreams,
    lwe_array_out: &mut CudaVec<T>,
    glwe_in: &CudaVec<T>,
    bootstrapping_key: &CudaVec<B>,
    bodies_count: u32,
    message_modulus: MessageModulus,
    carry_modulus: CarryModulus,
    encryption_glwe_dimension: GlweDimension,
    encryption_polynomial_size: PolynomialSize,
    compression_glwe_dimension: GlweDimension,
    compression_polynomial_size: PolynomialSize,
    lwe_dimension: LweDimension,
    pbs_base_log: DecompositionBaseLog,
    pbs_level: DecompositionLevelCount,
    storage_log_modulus: u32,
    vec_indexes: &[u32],
    num_lwes: u32,
) {
    assert_eq!(
        streams.gpu_indexes[0],
        lwe_array_out.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        glwe_in.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        bootstrapping_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    let mut mem_ptr: *mut i8 = std::ptr::null_mut();
    scratch_cuda_integer_decompress_radix_ciphertext_64(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
        encryption_glwe_dimension.0 as u32,
        encryption_polynomial_size.0 as u32,
        compression_glwe_dimension.0 as u32,
        compression_polynomial_size.0 as u32,
        lwe_dimension.0 as u32,
        pbs_level.0 as u32,
        pbs_base_log.0 as u32,
        num_lwes,
        message_modulus.0 as u32,
        carry_modulus.0 as u32,
        PBSType::Classical as u32,
        storage_log_modulus,
        bodies_count,
        true,
    );

    cuda_integer_decompress_radix_ciphertext_64(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        lwe_array_out.as_mut_c_ptr(0),
        glwe_in.as_c_ptr(0),
        vec_indexes.as_ptr(),
        vec_indexes.len() as u32,
        bootstrapping_key.ptr.as_ptr(),
        mem_ptr,
    );

    cleanup_cuda_integer_decompress_radix_ciphertext_64(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
    );
}

#[allow(clippy::too_many_arguments)]
/// # Safety
///
/// - [CudaStreams::synchronize] __must__ be called after this function as soon as synchronization
///   is required
pub unsafe fn unchecked_add_integer_radix_assign_async<T: UnsignedInteger>(
    streams: &CudaStreams,
    radix_lwe_left: &mut CudaVec<T>,
    radix_lwe_right: &CudaVec<T>,
    lwe_dimension: LweDimension,
    num_blocks: u32,
) {
    assert_eq!(
        streams.gpu_indexes[0],
        radix_lwe_left.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        radix_lwe_right.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    cuda_add_lwe_ciphertext_vector_64(
        streams.ptr[0],
        streams.gpu_indexes[0].0,
        radix_lwe_left.as_mut_c_ptr(0),
        radix_lwe_left.as_c_ptr(0),
        radix_lwe_right.as_c_ptr(0),
        lwe_dimension.0 as u32,
        num_blocks,
    );
}

#[allow(clippy::too_many_arguments)]
/// # Safety
///
/// - [CudaStreams::synchronize] __must__ be called after this function as soon as synchronization
///   is required
pub unsafe fn unchecked_mul_integer_radix_kb_assign_async<T: UnsignedInteger, B: Numeric>(
    streams: &CudaStreams,
    radix_lwe_left: &mut CudaVec<T>,
    is_boolean_left: bool,
    radix_lwe_right: &CudaVec<T>,
    is_boolean_right: bool,
    bootstrapping_key: &CudaVec<B>,
    keyswitch_key: &CudaVec<T>,
    message_modulus: MessageModulus,
    carry_modulus: CarryModulus,
    glwe_dimension: GlweDimension,
    lwe_dimension: LweDimension,
    polynomial_size: PolynomialSize,
    pbs_base_log: DecompositionBaseLog,
    pbs_level: DecompositionLevelCount,
    ks_base_log: DecompositionBaseLog,
    ks_level: DecompositionLevelCount,
    num_blocks: u32,
    pbs_type: PBSType,
    grouping_factor: LweBskGroupingFactor,
) {
    assert_eq!(
        streams.gpu_indexes[0],
        radix_lwe_left.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        radix_lwe_right.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        bootstrapping_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        keyswitch_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    let mut mem_ptr: *mut i8 = std::ptr::null_mut();
    scratch_cuda_integer_mult_radix_ciphertext_kb_64(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
        is_boolean_left,
        is_boolean_right,
        message_modulus.0 as u32,
        carry_modulus.0 as u32,
        glwe_dimension.0 as u32,
        lwe_dimension.0 as u32,
        polynomial_size.0 as u32,
        pbs_base_log.0 as u32,
        pbs_level.0 as u32,
        ks_base_log.0 as u32,
        ks_level.0 as u32,
        grouping_factor.0 as u32,
        num_blocks,
        pbs_type as u32,
        true,
    );
    cuda_integer_mult_radix_ciphertext_kb_64(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        radix_lwe_left.as_mut_c_ptr(0),
        radix_lwe_left.as_c_ptr(0),
        is_boolean_left,
        radix_lwe_right.as_c_ptr(0),
        is_boolean_right,
        bootstrapping_key.ptr.as_ptr(),
        keyswitch_key.ptr.as_ptr(),
        mem_ptr,
        polynomial_size.0 as u32,
        num_blocks,
    );
    cleanup_cuda_integer_mult(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
    );
}

#[allow(clippy::too_many_arguments)]
/// # Safety
///
/// - [CudaStreams::synchronize] __must__ be called after this function as soon as synchronization
///   is required
pub unsafe fn unchecked_bitop_integer_radix_kb_assign_async<T: UnsignedInteger, B: Numeric>(
    streams: &CudaStreams,
    radix_lwe_left: &mut CudaVec<T>,
    radix_lwe_right: &CudaVec<T>,
    bootstrapping_key: &CudaVec<B>,
    keyswitch_key: &CudaVec<T>,
    message_modulus: MessageModulus,
    carry_modulus: CarryModulus,
    glwe_dimension: GlweDimension,
    polynomial_size: PolynomialSize,
    big_lwe_dimension: LweDimension,
    small_lwe_dimension: LweDimension,
    ks_level: DecompositionLevelCount,
    ks_base_log: DecompositionBaseLog,
    pbs_level: DecompositionLevelCount,
    pbs_base_log: DecompositionBaseLog,
    op: BitOpType,
    num_blocks: u32,
    pbs_type: PBSType,
    grouping_factor: LweBskGroupingFactor,
) {
    assert_eq!(
        streams.gpu_indexes[0],
        radix_lwe_left.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        radix_lwe_right.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        bootstrapping_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        keyswitch_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    let mut mem_ptr: *mut i8 = std::ptr::null_mut();
    scratch_cuda_integer_radix_bitop_kb_64(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
        glwe_dimension.0 as u32,
        polynomial_size.0 as u32,
        big_lwe_dimension.0 as u32,
        small_lwe_dimension.0 as u32,
        ks_level.0 as u32,
        ks_base_log.0 as u32,
        pbs_level.0 as u32,
        pbs_base_log.0 as u32,
        grouping_factor.0 as u32,
        num_blocks,
        message_modulus.0 as u32,
        carry_modulus.0 as u32,
        pbs_type as u32,
        op as u32,
        true,
    );
    cuda_bitop_integer_radix_ciphertext_kb_64(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        radix_lwe_left.as_mut_c_ptr(0),
        radix_lwe_left.as_c_ptr(0),
        radix_lwe_right.as_c_ptr(0),
        mem_ptr,
        bootstrapping_key.ptr.as_ptr(),
        keyswitch_key.ptr.as_ptr(),
        num_blocks,
    );
    cleanup_cuda_integer_bitop(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
    );
}

#[allow(clippy::too_many_arguments)]
/// # Safety
///
/// - [CudaStreams::synchronize] __must__ be called after this function as soon as synchronization
///   is required
pub unsafe fn unchecked_scalar_bitop_integer_radix_kb_assign_async<
    T: UnsignedInteger,
    B: Numeric,
>(
    streams: &CudaStreams,
    radix_lwe: &mut CudaVec<T>,
    clear_blocks: &CudaVec<T>,
    bootstrapping_key: &CudaVec<B>,
    keyswitch_key: &CudaVec<T>,
    message_modulus: MessageModulus,
    carry_modulus: CarryModulus,
    glwe_dimension: GlweDimension,
    polynomial_size: PolynomialSize,
    big_lwe_dimension: LweDimension,
    small_lwe_dimension: LweDimension,
    ks_level: DecompositionLevelCount,
    ks_base_log: DecompositionBaseLog,
    pbs_level: DecompositionLevelCount,
    pbs_base_log: DecompositionBaseLog,
    op: BitOpType,
    num_blocks: u32,
    pbs_type: PBSType,
    grouping_factor: LweBskGroupingFactor,
) {
    assert_eq!(
        streams.gpu_indexes[0],
        radix_lwe.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        clear_blocks.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        bootstrapping_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        keyswitch_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    let mut mem_ptr: *mut i8 = std::ptr::null_mut();
    scratch_cuda_integer_radix_bitop_kb_64(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
        glwe_dimension.0 as u32,
        polynomial_size.0 as u32,
        big_lwe_dimension.0 as u32,
        small_lwe_dimension.0 as u32,
        ks_level.0 as u32,
        ks_base_log.0 as u32,
        pbs_level.0 as u32,
        pbs_base_log.0 as u32,
        grouping_factor.0 as u32,
        num_blocks,
        message_modulus.0 as u32,
        carry_modulus.0 as u32,
        pbs_type as u32,
        op as u32,
        true,
    );
    cuda_scalar_bitop_integer_radix_ciphertext_kb_64(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        radix_lwe.as_mut_c_ptr(0),
        radix_lwe.as_mut_c_ptr(0),
        clear_blocks.as_c_ptr(0),
        min(clear_blocks.len() as u32, num_blocks),
        mem_ptr,
        bootstrapping_key.ptr.as_ptr(),
        keyswitch_key.ptr.as_ptr(),
        num_blocks,
        op as u32,
    );
    cleanup_cuda_integer_bitop(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
    );
}

#[allow(clippy::too_many_arguments)]
/// # Safety
///
/// - [CudaStreams::synchronize] __must__ be called after this function as soon as synchronization
///   is required
pub unsafe fn unchecked_comparison_integer_radix_kb_async<T: UnsignedInteger, B: Numeric>(
    streams: &CudaStreams,
    radix_lwe_out: &mut CudaVec<T>,
    radix_lwe_left: &CudaVec<T>,
    radix_lwe_right: &CudaVec<T>,
    bootstrapping_key: &CudaVec<B>,
    keyswitch_key: &CudaVec<T>,
    message_modulus: MessageModulus,
    carry_modulus: CarryModulus,
    glwe_dimension: GlweDimension,
    polynomial_size: PolynomialSize,
    big_lwe_dimension: LweDimension,
    small_lwe_dimension: LweDimension,
    ks_level: DecompositionLevelCount,
    ks_base_log: DecompositionBaseLog,
    pbs_level: DecompositionLevelCount,
    pbs_base_log: DecompositionBaseLog,
    num_blocks: u32,
    op: ComparisonType,
    is_signed: bool,
    pbs_type: PBSType,
    grouping_factor: LweBskGroupingFactor,
) {
    assert_eq!(
        streams.gpu_indexes[0],
        radix_lwe_out.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        radix_lwe_left.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        radix_lwe_right.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        bootstrapping_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        keyswitch_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    let mut mem_ptr: *mut i8 = std::ptr::null_mut();
    scratch_cuda_integer_radix_comparison_kb_64(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
        glwe_dimension.0 as u32,
        polynomial_size.0 as u32,
        big_lwe_dimension.0 as u32,
        small_lwe_dimension.0 as u32,
        ks_level.0 as u32,
        ks_base_log.0 as u32,
        pbs_level.0 as u32,
        pbs_base_log.0 as u32,
        grouping_factor.0 as u32,
        num_blocks,
        message_modulus.0 as u32,
        carry_modulus.0 as u32,
        pbs_type as u32,
        op as u32,
        is_signed,
        true,
    );

    cuda_comparison_integer_radix_ciphertext_kb_64(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        radix_lwe_out.as_mut_c_ptr(0),
        radix_lwe_left.as_c_ptr(0),
        radix_lwe_right.as_c_ptr(0),
        mem_ptr,
        bootstrapping_key.ptr.as_ptr(),
        keyswitch_key.ptr.as_ptr(),
        num_blocks,
    );

    cleanup_cuda_integer_comparison(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
    );
}

#[allow(clippy::too_many_arguments)]
/// # Safety
///
/// - [CudaStreams::synchronize] __must__ be called after this function as soon as synchronization
///   is required
pub unsafe fn unchecked_scalar_comparison_integer_radix_kb_async<T: UnsignedInteger, B: Numeric>(
    streams: &CudaStreams,
    radix_lwe_out: &mut CudaVec<T>,
    radix_lwe_in: &CudaVec<T>,
    scalar_blocks: &CudaVec<T>,
    bootstrapping_key: &CudaVec<B>,
    keyswitch_key: &CudaVec<T>,
    message_modulus: MessageModulus,
    carry_modulus: CarryModulus,
    glwe_dimension: GlweDimension,
    polynomial_size: PolynomialSize,
    big_lwe_dimension: LweDimension,
    small_lwe_dimension: LweDimension,
    ks_level: DecompositionLevelCount,
    ks_base_log: DecompositionBaseLog,
    pbs_level: DecompositionLevelCount,
    pbs_base_log: DecompositionBaseLog,
    num_blocks: u32,
    num_scalar_blocks: u32,
    op: ComparisonType,
    signed_with_positive_scalar: bool,
    pbs_type: PBSType,
    grouping_factor: LweBskGroupingFactor,
) {
    assert_eq!(
        streams.gpu_indexes[0],
        radix_lwe_out.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        radix_lwe_in.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        scalar_blocks.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        bootstrapping_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        keyswitch_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    let mut mem_ptr: *mut i8 = std::ptr::null_mut();
    scratch_cuda_integer_radix_comparison_kb_64(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
        glwe_dimension.0 as u32,
        polynomial_size.0 as u32,
        big_lwe_dimension.0 as u32,
        small_lwe_dimension.0 as u32,
        ks_level.0 as u32,
        ks_base_log.0 as u32,
        pbs_level.0 as u32,
        pbs_base_log.0 as u32,
        grouping_factor.0 as u32,
        num_blocks,
        message_modulus.0 as u32,
        carry_modulus.0 as u32,
        pbs_type as u32,
        op as u32,
        signed_with_positive_scalar,
        true,
    );

    cuda_scalar_comparison_integer_radix_ciphertext_kb_64(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        radix_lwe_out.as_mut_c_ptr(0),
        radix_lwe_in.as_c_ptr(0),
        scalar_blocks.as_c_ptr(0),
        mem_ptr,
        bootstrapping_key.ptr.as_ptr(),
        keyswitch_key.ptr.as_ptr(),
        num_blocks,
        num_scalar_blocks,
    );

    cleanup_cuda_integer_comparison(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
    );
}

#[allow(clippy::too_many_arguments)]
/// # Safety
///
/// - [CudaStreams::synchronize] __must__ be called after this function as soon as synchronization
///   is required
pub unsafe fn full_propagate_assign_async<T: UnsignedInteger, B: Numeric>(
    streams: &CudaStreams,
    radix_lwe_input: &mut CudaVec<T>,
    bootstrapping_key: &CudaVec<B>,
    keyswitch_key: &CudaVec<T>,
    lwe_dimension: LweDimension,
    glwe_dimension: GlweDimension,
    polynomial_size: PolynomialSize,
    ks_level: DecompositionLevelCount,
    ks_base_log: DecompositionBaseLog,
    pbs_level: DecompositionLevelCount,
    pbs_base_log: DecompositionBaseLog,
    num_blocks: u32,
    message_modulus: MessageModulus,
    carry_modulus: CarryModulus,
    pbs_type: PBSType,
    grouping_factor: LweBskGroupingFactor,
) {
    assert_eq!(
        streams.gpu_indexes[0],
        radix_lwe_input.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        bootstrapping_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        keyswitch_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    let mut mem_ptr: *mut i8 = std::ptr::null_mut();
    scratch_cuda_full_propagation_64(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
        lwe_dimension.0 as u32,
        glwe_dimension.0 as u32,
        polynomial_size.0 as u32,
        ks_level.0 as u32,
        ks_base_log.0 as u32,
        pbs_level.0 as u32,
        pbs_base_log.0 as u32,
        grouping_factor.0 as u32,
        message_modulus.0 as u32,
        carry_modulus.0 as u32,
        pbs_type as u32,
        true,
    );
    cuda_full_propagation_64_inplace(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        radix_lwe_input.as_mut_c_ptr(0),
        mem_ptr,
        keyswitch_key.ptr.as_ptr(),
        bootstrapping_key.ptr.as_ptr(),
        num_blocks,
    );
    cleanup_cuda_full_propagation(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
    );
}

#[allow(clippy::too_many_arguments)]
/// # Safety
///
/// - [CudaStreams::synchronize] __must__ be called after this function as soon as synchronization
///   is required
pub(crate) unsafe fn propagate_single_carry_assign_async<T: UnsignedInteger, B: Numeric>(
    streams: &CudaStreams,
    radix_lwe_input: &mut CudaVec<T>,
    carry_out: &mut CudaVec<T>,
    carry_in: &CudaVec<T>,
    bootstrapping_key: &CudaVec<B>,
    keyswitch_key: &CudaVec<T>,
    lwe_dimension: LweDimension,
    glwe_dimension: GlweDimension,
    polynomial_size: PolynomialSize,
    ks_level: DecompositionLevelCount,
    ks_base_log: DecompositionBaseLog,
    pbs_level: DecompositionLevelCount,
    pbs_base_log: DecompositionBaseLog,
    num_blocks: u32,
    message_modulus: MessageModulus,
    carry_modulus: CarryModulus,
    pbs_type: PBSType,
    grouping_factor: LweBskGroupingFactor,
    requested_flag: OutputFlag,
    uses_carry: u32,
) {
    assert_eq!(
        streams.gpu_indexes[0],
        radix_lwe_input.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        bootstrapping_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        keyswitch_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    let mut mem_ptr: *mut i8 = std::ptr::null_mut();
    let big_lwe_dimension: u32 = glwe_dimension.0 as u32 * polynomial_size.0 as u32;
    scratch_cuda_propagate_single_carry_kb_64_inplace(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
        glwe_dimension.0 as u32,
        polynomial_size.0 as u32,
        big_lwe_dimension,
        lwe_dimension.0 as u32,
        ks_level.0 as u32,
        ks_base_log.0 as u32,
        pbs_level.0 as u32,
        pbs_base_log.0 as u32,
        grouping_factor.0 as u32,
        num_blocks,
        message_modulus.0 as u32,
        carry_modulus.0 as u32,
        pbs_type as u32,
        requested_flag as u32,
        uses_carry,
        true,
    );
    cuda_propagate_single_carry_kb_64_inplace(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        radix_lwe_input.as_mut_c_ptr(0),
        carry_out.as_mut_c_ptr(0),
        carry_in.as_c_ptr(0),
        mem_ptr,
        bootstrapping_key.ptr.as_ptr(),
        keyswitch_key.ptr.as_ptr(),
        num_blocks,
        requested_flag as u32,
        uses_carry,
    );
    cleanup_cuda_propagate_single_carry(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
    );
}

#[allow(clippy::too_many_arguments)]
/// # Safety
///
/// - [CudaStreams::synchronize] __must__ be called after this function as soon as synchronization
///   is required
pub(crate) unsafe fn add_and_propagate_single_carry_assign_async<T: UnsignedInteger, B: Numeric>(
    streams: &CudaStreams,
    radix_lwe_lhs_input: &mut CudaVec<T>,
    radix_lwe_rhs_input: &CudaVec<T>,
    carry_out: &mut CudaVec<T>,
    carry_in: &CudaVec<T>,
    bootstrapping_key: &CudaVec<B>,
    keyswitch_key: &CudaVec<T>,
    lwe_dimension: LweDimension,
    glwe_dimension: GlweDimension,
    polynomial_size: PolynomialSize,
    ks_level: DecompositionLevelCount,
    ks_base_log: DecompositionBaseLog,
    pbs_level: DecompositionLevelCount,
    pbs_base_log: DecompositionBaseLog,
    num_blocks: u32,
    message_modulus: MessageModulus,
    carry_modulus: CarryModulus,
    pbs_type: PBSType,
    grouping_factor: LweBskGroupingFactor,
    requested_flag: OutputFlag,
    uses_carry: u32,
) {
    assert_eq!(
        streams.gpu_indexes[0],
        radix_lwe_lhs_input.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        radix_lwe_rhs_input.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        bootstrapping_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        keyswitch_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    let mut mem_ptr: *mut i8 = std::ptr::null_mut();
    let big_lwe_dimension: u32 = glwe_dimension.0 as u32 * polynomial_size.0 as u32;
    scratch_cuda_add_and_propagate_single_carry_kb_64_inplace(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
        glwe_dimension.0 as u32,
        polynomial_size.0 as u32,
        big_lwe_dimension,
        lwe_dimension.0 as u32,
        ks_level.0 as u32,
        ks_base_log.0 as u32,
        pbs_level.0 as u32,
        pbs_base_log.0 as u32,
        grouping_factor.0 as u32,
        num_blocks,
        message_modulus.0 as u32,
        carry_modulus.0 as u32,
        pbs_type as u32,
        requested_flag as u32,
        uses_carry,
        true,
    );
    cuda_add_and_propagate_single_carry_kb_64_inplace(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        radix_lwe_lhs_input.as_mut_c_ptr(0),
        radix_lwe_rhs_input.as_c_ptr(0),
        carry_out.as_mut_c_ptr(0),
        carry_in.as_c_ptr(0),
        mem_ptr,
        bootstrapping_key.ptr.as_ptr(),
        keyswitch_key.ptr.as_ptr(),
        num_blocks,
        requested_flag as u32,
        uses_carry,
    );
    cleanup_cuda_add_and_propagate_single_carry(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
    );
}

#[allow(clippy::too_many_arguments)]
/// # Safety
///
/// - [CudaStreams::synchronize] __must__ be called after this function as soon as synchronization
///   is required
pub unsafe fn unchecked_scalar_left_shift_integer_radix_kb_assign_async<
    T: UnsignedInteger,
    B: Numeric,
>(
    streams: &CudaStreams,
    radix_lwe_left: &mut CudaVec<T>,
    shift: u32,
    bootstrapping_key: &CudaVec<B>,
    keyswitch_key: &CudaVec<T>,
    message_modulus: MessageModulus,
    carry_modulus: CarryModulus,
    glwe_dimension: GlweDimension,
    polynomial_size: PolynomialSize,
    big_lwe_dimension: LweDimension,
    small_lwe_dimension: LweDimension,
    ks_level: DecompositionLevelCount,
    ks_base_log: DecompositionBaseLog,
    pbs_level: DecompositionLevelCount,
    pbs_base_log: DecompositionBaseLog,
    num_blocks: u32,
    pbs_type: PBSType,
    grouping_factor: LweBskGroupingFactor,
) {
    assert_eq!(
        streams.gpu_indexes[0],
        radix_lwe_left.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        bootstrapping_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        keyswitch_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    let mut mem_ptr: *mut i8 = std::ptr::null_mut();
    scratch_cuda_integer_radix_logical_scalar_shift_kb_64(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
        glwe_dimension.0 as u32,
        polynomial_size.0 as u32,
        big_lwe_dimension.0 as u32,
        small_lwe_dimension.0 as u32,
        ks_level.0 as u32,
        ks_base_log.0 as u32,
        pbs_level.0 as u32,
        pbs_base_log.0 as u32,
        grouping_factor.0 as u32,
        num_blocks,
        message_modulus.0 as u32,
        carry_modulus.0 as u32,
        pbs_type as u32,
        ShiftRotateType::LeftShift as u32,
        true,
    );
    cuda_integer_radix_logical_scalar_shift_kb_64_inplace(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        radix_lwe_left.as_mut_c_ptr(0),
        shift,
        mem_ptr,
        bootstrapping_key.ptr.as_ptr(),
        keyswitch_key.ptr.as_ptr(),
        num_blocks,
    );
    cleanup_cuda_integer_radix_logical_scalar_shift(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
    );
}

#[allow(clippy::too_many_arguments)]
/// # Safety
///
/// - [CudaStreams::synchronize] __must__ be called after this function as soon as synchronization
///   is required
pub unsafe fn unchecked_scalar_logical_right_shift_integer_radix_kb_assign_async<
    T: UnsignedInteger,
    B: Numeric,
>(
    streams: &CudaStreams,
    radix_lwe_left: &mut CudaVec<T>,
    shift: u32,
    bootstrapping_key: &CudaVec<B>,
    keyswitch_key: &CudaVec<T>,
    message_modulus: MessageModulus,
    carry_modulus: CarryModulus,
    glwe_dimension: GlweDimension,
    polynomial_size: PolynomialSize,
    big_lwe_dimension: LweDimension,
    small_lwe_dimension: LweDimension,
    ks_level: DecompositionLevelCount,
    ks_base_log: DecompositionBaseLog,
    pbs_level: DecompositionLevelCount,
    pbs_base_log: DecompositionBaseLog,
    num_blocks: u32,
    pbs_type: PBSType,
    grouping_factor: LweBskGroupingFactor,
) {
    assert_eq!(
        streams.gpu_indexes[0],
        radix_lwe_left.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        bootstrapping_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        keyswitch_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    let mut mem_ptr: *mut i8 = std::ptr::null_mut();
    scratch_cuda_integer_radix_logical_scalar_shift_kb_64(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
        glwe_dimension.0 as u32,
        polynomial_size.0 as u32,
        big_lwe_dimension.0 as u32,
        small_lwe_dimension.0 as u32,
        ks_level.0 as u32,
        ks_base_log.0 as u32,
        pbs_level.0 as u32,
        pbs_base_log.0 as u32,
        grouping_factor.0 as u32,
        num_blocks,
        message_modulus.0 as u32,
        carry_modulus.0 as u32,
        pbs_type as u32,
        ShiftRotateType::RightShift as u32,
        true,
    );
    cuda_integer_radix_logical_scalar_shift_kb_64_inplace(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        radix_lwe_left.as_mut_c_ptr(0),
        shift,
        mem_ptr,
        bootstrapping_key.ptr.as_ptr(),
        keyswitch_key.ptr.as_ptr(),
        num_blocks,
    );
    cleanup_cuda_integer_radix_logical_scalar_shift(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
    );
}

#[allow(clippy::too_many_arguments)]
/// # Safety
///
/// - [CudaStreams::synchronize] __must__ be called after this function as soon as synchronization
///   is required
pub unsafe fn unchecked_scalar_arithmetic_right_shift_integer_radix_kb_assign_async<
    T: UnsignedInteger,
    B: Numeric,
>(
    streams: &CudaStreams,
    radix_lwe_left: &mut CudaVec<T>,
    shift: u32,
    bootstrapping_key: &CudaVec<B>,
    keyswitch_key: &CudaVec<T>,
    message_modulus: MessageModulus,
    carry_modulus: CarryModulus,
    glwe_dimension: GlweDimension,
    polynomial_size: PolynomialSize,
    big_lwe_dimension: LweDimension,
    small_lwe_dimension: LweDimension,
    ks_level: DecompositionLevelCount,
    ks_base_log: DecompositionBaseLog,
    pbs_level: DecompositionLevelCount,
    pbs_base_log: DecompositionBaseLog,
    num_blocks: u32,
    pbs_type: PBSType,
    grouping_factor: LweBskGroupingFactor,
) {
    assert_eq!(
        streams.gpu_indexes[0],
        radix_lwe_left.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        bootstrapping_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        keyswitch_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    let mut mem_ptr: *mut i8 = std::ptr::null_mut();
    scratch_cuda_integer_radix_arithmetic_scalar_shift_kb_64(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
        glwe_dimension.0 as u32,
        polynomial_size.0 as u32,
        big_lwe_dimension.0 as u32,
        small_lwe_dimension.0 as u32,
        ks_level.0 as u32,
        ks_base_log.0 as u32,
        pbs_level.0 as u32,
        pbs_base_log.0 as u32,
        grouping_factor.0 as u32,
        num_blocks,
        message_modulus.0 as u32,
        carry_modulus.0 as u32,
        pbs_type as u32,
        ShiftRotateType::RightShift as u32,
        true,
    );
    cuda_integer_radix_arithmetic_scalar_shift_kb_64_inplace(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        radix_lwe_left.as_mut_c_ptr(0),
        shift,
        mem_ptr,
        bootstrapping_key.ptr.as_ptr(),
        keyswitch_key.ptr.as_ptr(),
        num_blocks,
    );
    cleanup_cuda_integer_radix_arithmetic_scalar_shift(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
    );
}

#[allow(clippy::too_many_arguments)]
/// # Safety
///
/// - [CudaStreams::synchronize] __must__ be called after this function as soon as synchronization
///   is required
pub unsafe fn unchecked_right_shift_integer_radix_kb_assign_async<
    T: UnsignedInteger,
    B: Numeric,
>(
    streams: &CudaStreams,
    radix_lwe_left: &mut CudaVec<T>,
    radix_shift: &CudaVec<T>,
    bootstrapping_key: &CudaVec<B>,
    keyswitch_key: &CudaVec<T>,
    message_modulus: MessageModulus,
    carry_modulus: CarryModulus,
    glwe_dimension: GlweDimension,
    polynomial_size: PolynomialSize,
    big_lwe_dimension: LweDimension,
    small_lwe_dimension: LweDimension,
    ks_level: DecompositionLevelCount,
    ks_base_log: DecompositionBaseLog,
    pbs_level: DecompositionLevelCount,
    pbs_base_log: DecompositionBaseLog,
    num_blocks: u32,
    is_signed: bool,
    pbs_type: PBSType,
    grouping_factor: LweBskGroupingFactor,
) {
    assert_eq!(
        streams.gpu_indexes[0],
        radix_lwe_left.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        radix_shift.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        bootstrapping_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        keyswitch_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    let mut mem_ptr: *mut i8 = std::ptr::null_mut();
    scratch_cuda_integer_radix_shift_and_rotate_kb_64(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
        glwe_dimension.0 as u32,
        polynomial_size.0 as u32,
        big_lwe_dimension.0 as u32,
        small_lwe_dimension.0 as u32,
        ks_level.0 as u32,
        ks_base_log.0 as u32,
        pbs_level.0 as u32,
        pbs_base_log.0 as u32,
        grouping_factor.0 as u32,
        num_blocks,
        message_modulus.0 as u32,
        carry_modulus.0 as u32,
        pbs_type as u32,
        ShiftRotateType::RightShift as u32,
        is_signed,
        true,
    );
    cuda_integer_radix_shift_and_rotate_kb_64_inplace(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        radix_lwe_left.as_mut_c_ptr(0),
        radix_shift.as_c_ptr(0),
        mem_ptr,
        bootstrapping_key.ptr.as_ptr(),
        keyswitch_key.ptr.as_ptr(),
        num_blocks,
    );
    cleanup_cuda_integer_radix_shift_and_rotate(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
    );
}

#[allow(clippy::too_many_arguments)]
/// # Safety
///
/// - [CudaStreams::synchronize] __must__ be called after this function as soon as synchronization
///   is required
pub unsafe fn unchecked_left_shift_integer_radix_kb_assign_async<T: UnsignedInteger, B: Numeric>(
    streams: &CudaStreams,
    radix_lwe_left: &mut CudaVec<T>,
    radix_shift: &CudaVec<T>,
    bootstrapping_key: &CudaVec<B>,
    keyswitch_key: &CudaVec<T>,
    message_modulus: MessageModulus,
    carry_modulus: CarryModulus,
    glwe_dimension: GlweDimension,
    polynomial_size: PolynomialSize,
    big_lwe_dimension: LweDimension,
    small_lwe_dimension: LweDimension,
    ks_level: DecompositionLevelCount,
    ks_base_log: DecompositionBaseLog,
    pbs_level: DecompositionLevelCount,
    pbs_base_log: DecompositionBaseLog,
    num_blocks: u32,
    is_signed: bool,
    pbs_type: PBSType,
    grouping_factor: LweBskGroupingFactor,
) {
    assert_eq!(
        streams.gpu_indexes[0],
        radix_lwe_left.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        radix_shift.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        bootstrapping_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        keyswitch_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    let mut mem_ptr: *mut i8 = std::ptr::null_mut();
    scratch_cuda_integer_radix_shift_and_rotate_kb_64(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
        glwe_dimension.0 as u32,
        polynomial_size.0 as u32,
        big_lwe_dimension.0 as u32,
        small_lwe_dimension.0 as u32,
        ks_level.0 as u32,
        ks_base_log.0 as u32,
        pbs_level.0 as u32,
        pbs_base_log.0 as u32,
        grouping_factor.0 as u32,
        num_blocks,
        message_modulus.0 as u32,
        carry_modulus.0 as u32,
        pbs_type as u32,
        ShiftRotateType::LeftShift as u32,
        is_signed,
        true,
    );
    cuda_integer_radix_shift_and_rotate_kb_64_inplace(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        radix_lwe_left.as_mut_c_ptr(0),
        radix_shift.as_c_ptr(0),
        mem_ptr,
        bootstrapping_key.ptr.as_ptr(),
        keyswitch_key.ptr.as_ptr(),
        num_blocks,
    );
    cleanup_cuda_integer_radix_shift_and_rotate(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
    );
}

#[allow(clippy::too_many_arguments)]
/// # Safety
///
/// - [CudaStreams::synchronize] __must__ be called after this function as soon as synchronization
///   is required
pub unsafe fn unchecked_rotate_right_integer_radix_kb_assign_async<
    T: UnsignedInteger,
    B: Numeric,
>(
    streams: &CudaStreams,
    radix_lwe_left: &mut CudaVec<T>,
    radix_shift: &CudaVec<T>,
    bootstrapping_key: &CudaVec<B>,
    keyswitch_key: &CudaVec<T>,
    message_modulus: MessageModulus,
    carry_modulus: CarryModulus,
    glwe_dimension: GlweDimension,
    polynomial_size: PolynomialSize,
    big_lwe_dimension: LweDimension,
    small_lwe_dimension: LweDimension,
    ks_level: DecompositionLevelCount,
    ks_base_log: DecompositionBaseLog,
    pbs_level: DecompositionLevelCount,
    pbs_base_log: DecompositionBaseLog,
    num_blocks: u32,
    is_signed: bool,
    pbs_type: PBSType,
    grouping_factor: LweBskGroupingFactor,
) {
    assert_eq!(
        streams.gpu_indexes[0],
        radix_lwe_left.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        radix_shift.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        bootstrapping_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        keyswitch_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    let mut mem_ptr: *mut i8 = std::ptr::null_mut();
    scratch_cuda_integer_radix_shift_and_rotate_kb_64(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
        glwe_dimension.0 as u32,
        polynomial_size.0 as u32,
        big_lwe_dimension.0 as u32,
        small_lwe_dimension.0 as u32,
        ks_level.0 as u32,
        ks_base_log.0 as u32,
        pbs_level.0 as u32,
        pbs_base_log.0 as u32,
        grouping_factor.0 as u32,
        num_blocks,
        message_modulus.0 as u32,
        carry_modulus.0 as u32,
        pbs_type as u32,
        ShiftRotateType::RightRotate as u32,
        is_signed,
        true,
    );
    cuda_integer_radix_shift_and_rotate_kb_64_inplace(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        radix_lwe_left.as_mut_c_ptr(0),
        radix_shift.as_c_ptr(0),
        mem_ptr,
        bootstrapping_key.ptr.as_ptr(),
        keyswitch_key.ptr.as_ptr(),
        num_blocks,
    );
    cleanup_cuda_integer_radix_shift_and_rotate(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
    );
}

#[allow(clippy::too_many_arguments)]
/// # Safety
///
/// - [CudaStreams::synchronize] __must__ be called after this function as soon as synchronization
///   is required
pub unsafe fn unchecked_rotate_left_integer_radix_kb_assign_async<
    T: UnsignedInteger,
    B: Numeric,
>(
    streams: &CudaStreams,
    radix_lwe_left: &mut CudaVec<T>,
    radix_shift: &CudaVec<T>,
    bootstrapping_key: &CudaVec<B>,
    keyswitch_key: &CudaVec<T>,
    message_modulus: MessageModulus,
    carry_modulus: CarryModulus,
    glwe_dimension: GlweDimension,
    polynomial_size: PolynomialSize,
    big_lwe_dimension: LweDimension,
    small_lwe_dimension: LweDimension,
    ks_level: DecompositionLevelCount,
    ks_base_log: DecompositionBaseLog,
    pbs_level: DecompositionLevelCount,
    pbs_base_log: DecompositionBaseLog,
    num_blocks: u32,
    is_signed: bool,
    pbs_type: PBSType,
    grouping_factor: LweBskGroupingFactor,
) {
    assert_eq!(
        streams.gpu_indexes[0],
        radix_lwe_left.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        radix_shift.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        bootstrapping_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        keyswitch_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    let mut mem_ptr: *mut i8 = std::ptr::null_mut();
    scratch_cuda_integer_radix_shift_and_rotate_kb_64(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
        glwe_dimension.0 as u32,
        polynomial_size.0 as u32,
        big_lwe_dimension.0 as u32,
        small_lwe_dimension.0 as u32,
        ks_level.0 as u32,
        ks_base_log.0 as u32,
        pbs_level.0 as u32,
        pbs_base_log.0 as u32,
        grouping_factor.0 as u32,
        num_blocks,
        message_modulus.0 as u32,
        carry_modulus.0 as u32,
        pbs_type as u32,
        ShiftRotateType::LeftRotate as u32,
        is_signed,
        true,
    );
    cuda_integer_radix_shift_and_rotate_kb_64_inplace(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        radix_lwe_left.as_mut_c_ptr(0),
        radix_shift.as_c_ptr(0),
        mem_ptr,
        bootstrapping_key.ptr.as_ptr(),
        keyswitch_key.ptr.as_ptr(),
        num_blocks,
    );
    cleanup_cuda_integer_radix_shift_and_rotate(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
    );
}

#[allow(clippy::too_many_arguments)]
/// # Safety
///
/// - [CudaStreams::synchronize] __must__ be called after this function as soon as synchronization
///   is required
pub unsafe fn unchecked_cmux_integer_radix_kb_async<T: UnsignedInteger, B: Numeric>(
    streams: &CudaStreams,
    radix_lwe_out: &mut CudaVec<T>,
    radix_lwe_condition: &CudaVec<T>,
    radix_lwe_true: &CudaVec<T>,
    radix_lwe_false: &CudaVec<T>,
    bootstrapping_key: &CudaVec<B>,
    keyswitch_key: &CudaVec<T>,
    message_modulus: MessageModulus,
    carry_modulus: CarryModulus,
    glwe_dimension: GlweDimension,
    polynomial_size: PolynomialSize,
    big_lwe_dimension: LweDimension,
    small_lwe_dimension: LweDimension,
    ks_level: DecompositionLevelCount,
    ks_base_log: DecompositionBaseLog,
    pbs_level: DecompositionLevelCount,
    pbs_base_log: DecompositionBaseLog,
    num_blocks: u32,
    pbs_type: PBSType,
    grouping_factor: LweBskGroupingFactor,
) {
    assert_eq!(
        streams.gpu_indexes[0],
        radix_lwe_out.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        radix_lwe_condition.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        radix_lwe_true.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        radix_lwe_false.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        bootstrapping_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        keyswitch_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    let mut mem_ptr: *mut i8 = std::ptr::null_mut();
    scratch_cuda_integer_radix_cmux_kb_64(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
        glwe_dimension.0 as u32,
        polynomial_size.0 as u32,
        big_lwe_dimension.0 as u32,
        small_lwe_dimension.0 as u32,
        ks_level.0 as u32,
        ks_base_log.0 as u32,
        pbs_level.0 as u32,
        pbs_base_log.0 as u32,
        grouping_factor.0 as u32,
        num_blocks,
        message_modulus.0 as u32,
        carry_modulus.0 as u32,
        pbs_type as u32,
        true,
    );
    cuda_cmux_integer_radix_ciphertext_kb_64(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        radix_lwe_out.as_mut_c_ptr(0),
        radix_lwe_condition.as_c_ptr(0),
        radix_lwe_true.as_c_ptr(0),
        radix_lwe_false.as_c_ptr(0),
        mem_ptr,
        bootstrapping_key.ptr.as_ptr(),
        keyswitch_key.ptr.as_ptr(),
        num_blocks,
    );
    cleanup_cuda_integer_radix_cmux(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
    );
}

#[allow(clippy::too_many_arguments)]
/// # Safety
///
/// - [CudaStreams::synchronize] __must__ be called after this function as soon as synchronization
///   is required
pub unsafe fn unchecked_scalar_rotate_left_integer_radix_kb_assign_async<
    T: UnsignedInteger,
    B: Numeric,
>(
    streams: &CudaStreams,
    radix_lwe_left: &mut CudaVec<T>,
    n: u32,
    bootstrapping_key: &CudaVec<B>,
    keyswitch_key: &CudaVec<T>,
    message_modulus: MessageModulus,
    carry_modulus: CarryModulus,
    glwe_dimension: GlweDimension,
    polynomial_size: PolynomialSize,
    big_lwe_dimension: LweDimension,
    small_lwe_dimension: LweDimension,
    ks_level: DecompositionLevelCount,
    ks_base_log: DecompositionBaseLog,
    pbs_level: DecompositionLevelCount,
    pbs_base_log: DecompositionBaseLog,
    num_blocks: u32,
    pbs_type: PBSType,
    grouping_factor: LweBskGroupingFactor,
) {
    assert_eq!(
        streams.gpu_indexes[0],
        radix_lwe_left.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        bootstrapping_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        keyswitch_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    let mut mem_ptr: *mut i8 = std::ptr::null_mut();
    scratch_cuda_integer_radix_scalar_rotate_kb_64(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
        glwe_dimension.0 as u32,
        polynomial_size.0 as u32,
        big_lwe_dimension.0 as u32,
        small_lwe_dimension.0 as u32,
        ks_level.0 as u32,
        ks_base_log.0 as u32,
        pbs_level.0 as u32,
        pbs_base_log.0 as u32,
        grouping_factor.0 as u32,
        num_blocks,
        message_modulus.0 as u32,
        carry_modulus.0 as u32,
        pbs_type as u32,
        ShiftRotateType::LeftShift as u32,
        true,
    );
    cuda_integer_radix_scalar_rotate_kb_64_inplace(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        radix_lwe_left.as_mut_c_ptr(0),
        n,
        mem_ptr,
        bootstrapping_key.ptr.as_ptr(),
        keyswitch_key.ptr.as_ptr(),
        num_blocks,
    );
    cleanup_cuda_integer_radix_scalar_rotate(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
    );
}

#[allow(clippy::too_many_arguments)]
/// # Safety
///
/// - [CudaStreams::synchronize] __must__ be called after this function as soon as synchronization
///   is required
pub unsafe fn unchecked_scalar_rotate_right_integer_radix_kb_assign_async<
    T: UnsignedInteger,
    B: Numeric,
>(
    streams: &CudaStreams,
    radix_lwe_left: &mut CudaVec<T>,
    n: u32,
    bootstrapping_key: &CudaVec<B>,
    keyswitch_key: &CudaVec<T>,
    message_modulus: MessageModulus,
    carry_modulus: CarryModulus,
    glwe_dimension: GlweDimension,
    polynomial_size: PolynomialSize,
    big_lwe_dimension: LweDimension,
    small_lwe_dimension: LweDimension,
    ks_level: DecompositionLevelCount,
    ks_base_log: DecompositionBaseLog,
    pbs_level: DecompositionLevelCount,
    pbs_base_log: DecompositionBaseLog,
    num_blocks: u32,
    pbs_type: PBSType,
    grouping_factor: LweBskGroupingFactor,
) {
    assert_eq!(
        streams.gpu_indexes[0],
        radix_lwe_left.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        bootstrapping_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        keyswitch_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    let mut mem_ptr: *mut i8 = std::ptr::null_mut();
    scratch_cuda_integer_radix_scalar_rotate_kb_64(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
        glwe_dimension.0 as u32,
        polynomial_size.0 as u32,
        big_lwe_dimension.0 as u32,
        small_lwe_dimension.0 as u32,
        ks_level.0 as u32,
        ks_base_log.0 as u32,
        pbs_level.0 as u32,
        pbs_base_log.0 as u32,
        grouping_factor.0 as u32,
        num_blocks,
        message_modulus.0 as u32,
        carry_modulus.0 as u32,
        pbs_type as u32,
        ShiftRotateType::RightShift as u32,
        true,
    );
    cuda_integer_radix_scalar_rotate_kb_64_inplace(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        radix_lwe_left.as_mut_c_ptr(0),
        n,
        mem_ptr,
        bootstrapping_key.ptr.as_ptr(),
        keyswitch_key.ptr.as_ptr(),
        num_blocks,
    );
    cleanup_cuda_integer_radix_scalar_rotate(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
    );
}

#[allow(clippy::too_many_arguments)]
/// # Safety
///
/// - [CudaStreams::synchronize] __must__ be called after this function as soon as synchronization
///   is required
pub unsafe fn unchecked_partial_sum_ciphertexts_integer_radix_kb_assign_async<
    T: UnsignedInteger,
    B: Numeric,
>(
    streams: &CudaStreams,
    result: &mut CudaVec<T>,
    radix_list: &mut CudaVec<T>,
    bootstrapping_key: &CudaVec<B>,
    keyswitch_key: &CudaVec<T>,
    message_modulus: MessageModulus,
    carry_modulus: CarryModulus,
    glwe_dimension: GlweDimension,
    polynomial_size: PolynomialSize,
    lwe_dimension: LweDimension,
    ks_level: DecompositionLevelCount,
    ks_base_log: DecompositionBaseLog,
    pbs_level: DecompositionLevelCount,
    pbs_base_log: DecompositionBaseLog,
    num_blocks: u32,
    num_radixes: u32,
    pbs_type: PBSType,
    grouping_factor: LweBskGroupingFactor,
) {
    assert_eq!(
        streams.gpu_indexes[0],
        result.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        radix_list.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        bootstrapping_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        keyswitch_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    let mut mem_ptr: *mut i8 = std::ptr::null_mut();
    scratch_cuda_integer_radix_partial_sum_ciphertexts_vec_kb_64(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
        glwe_dimension.0 as u32,
        polynomial_size.0 as u32,
        lwe_dimension.0 as u32,
        ks_level.0 as u32,
        ks_base_log.0 as u32,
        pbs_level.0 as u32,
        pbs_base_log.0 as u32,
        grouping_factor.0 as u32,
        num_blocks,
        num_radixes,
        message_modulus.0 as u32,
        carry_modulus.0 as u32,
        pbs_type as u32,
        true,
    );
    cuda_integer_radix_partial_sum_ciphertexts_vec_kb_64(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        result.as_mut_c_ptr(0),
        radix_list.as_mut_c_ptr(0),
        num_radixes,
        mem_ptr,
        bootstrapping_key.ptr.as_ptr(),
        keyswitch_key.ptr.as_ptr(),
        num_blocks,
    );
    cleanup_cuda_integer_radix_partial_sum_ciphertexts_vec(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
    );
}

#[allow(clippy::too_many_arguments)]
/// # Safety
///
/// - [CudaStreams::synchronize] __must__ be called after this function as soon as synchronization
///   is required
pub unsafe fn apply_univariate_lut_kb_async<T: UnsignedInteger, B: Numeric>(
    streams: &CudaStreams,
    radix_lwe_output: &mut CudaSliceMut<T>,
    radix_lwe_input: &CudaSlice<T>,
    input_lut: &[T],
    bootstrapping_key: &CudaVec<B>,
    keyswitch_key: &CudaVec<T>,
    lwe_dimension: LweDimension,
    glwe_dimension: GlweDimension,
    polynomial_size: PolynomialSize,
    ks_level: DecompositionLevelCount,
    ks_base_log: DecompositionBaseLog,
    pbs_level: DecompositionLevelCount,
    pbs_base_log: DecompositionBaseLog,
    num_blocks: u32,
    message_modulus: MessageModulus,
    carry_modulus: CarryModulus,
    pbs_type: PBSType,
    grouping_factor: LweBskGroupingFactor,
) {
    assert_eq!(
        streams.gpu_indexes[0],
        radix_lwe_input.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        radix_lwe_output.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        bootstrapping_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        keyswitch_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    let mut mem_ptr: *mut i8 = std::ptr::null_mut();
    scratch_cuda_apply_univariate_lut_kb_64(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
        input_lut.as_ptr().cast(),
        lwe_dimension.0 as u32,
        glwe_dimension.0 as u32,
        polynomial_size.0 as u32,
        ks_level.0 as u32,
        ks_base_log.0 as u32,
        pbs_level.0 as u32,
        pbs_base_log.0 as u32,
        grouping_factor.0 as u32,
        num_blocks,
        message_modulus.0 as u32,
        carry_modulus.0 as u32,
        pbs_type as u32,
        true,
    );
    cuda_apply_univariate_lut_kb_64(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        radix_lwe_output.as_mut_c_ptr(0),
        radix_lwe_input.as_c_ptr(0),
        mem_ptr,
        keyswitch_key.ptr.as_ptr(),
        bootstrapping_key.ptr.as_ptr(),
        num_blocks,
    );
    cleanup_cuda_apply_univariate_lut_kb_64(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
    );
}

#[allow(clippy::too_many_arguments)]
/// # Safety
///
/// - [CudaStreams::synchronize] __must__ be called after this function as soon as synchronization
///   is required
pub unsafe fn apply_many_univariate_lut_kb_async<T: UnsignedInteger, B: Numeric>(
    streams: &CudaStreams,
    radix_lwe_output: &mut CudaSliceMut<T>,
    radix_lwe_input: &CudaSlice<T>,
    input_lut: &[T],
    bootstrapping_key: &CudaVec<B>,
    keyswitch_key: &CudaVec<T>,
    lwe_dimension: LweDimension,
    glwe_dimension: GlweDimension,
    polynomial_size: PolynomialSize,
    ks_level: DecompositionLevelCount,
    ks_base_log: DecompositionBaseLog,
    pbs_level: DecompositionLevelCount,
    pbs_base_log: DecompositionBaseLog,
    num_blocks: u32,
    message_modulus: MessageModulus,
    carry_modulus: CarryModulus,
    pbs_type: PBSType,
    grouping_factor: LweBskGroupingFactor,
    num_many_lut: u32,
    lut_stride: u32,
) {
    assert_eq!(
        streams.gpu_indexes[0],
        radix_lwe_input.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        radix_lwe_output.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        bootstrapping_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        keyswitch_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    let mut mem_ptr: *mut i8 = std::ptr::null_mut();
    scratch_cuda_apply_many_univariate_lut_kb_64(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
        input_lut.as_ptr().cast(),
        lwe_dimension.0 as u32,
        glwe_dimension.0 as u32,
        polynomial_size.0 as u32,
        ks_level.0 as u32,
        ks_base_log.0 as u32,
        pbs_level.0 as u32,
        pbs_base_log.0 as u32,
        grouping_factor.0 as u32,
        num_blocks,
        message_modulus.0 as u32,
        carry_modulus.0 as u32,
        pbs_type as u32,
        num_many_lut,
        true,
    );
    cuda_apply_many_univariate_lut_kb_64(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        radix_lwe_output.as_mut_c_ptr(0),
        radix_lwe_input.as_c_ptr(0),
        mem_ptr,
        keyswitch_key.ptr.as_ptr(),
        bootstrapping_key.ptr.as_ptr(),
        num_blocks,
        num_many_lut,
        lut_stride,
    );
    cleanup_cuda_apply_univariate_lut_kb_64(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
    );
}

#[allow(clippy::too_many_arguments)]
/// # Safety
///
/// - [CudaStreams::synchronize] __must__ be called after this function as soon as synchronization
///   is required
pub unsafe fn apply_bivariate_lut_kb_async<T: UnsignedInteger, B: Numeric>(
    streams: &CudaStreams,
    radix_lwe_output: &mut CudaSliceMut<T>,
    radix_lwe_input_1: &CudaVec<T>,
    radix_lwe_input_2: &CudaVec<T>,
    input_lut: &[T],
    bootstrapping_key: &CudaVec<B>,
    keyswitch_key: &CudaVec<T>,
    lwe_dimension: LweDimension,
    glwe_dimension: GlweDimension,
    polynomial_size: PolynomialSize,
    ks_level: DecompositionLevelCount,
    ks_base_log: DecompositionBaseLog,
    pbs_level: DecompositionLevelCount,
    pbs_base_log: DecompositionBaseLog,
    num_blocks: u32,
    message_modulus: MessageModulus,
    carry_modulus: CarryModulus,
    pbs_type: PBSType,
    grouping_factor: LweBskGroupingFactor,
    shift: u32,
) {
    assert_eq!(
        streams.gpu_indexes[0],
        radix_lwe_input_1.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        radix_lwe_input_2.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        radix_lwe_output.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        bootstrapping_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        keyswitch_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    let mut mem_ptr: *mut i8 = std::ptr::null_mut();
    scratch_cuda_apply_bivariate_lut_kb_64(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
        input_lut.as_ptr().cast(),
        lwe_dimension.0 as u32,
        glwe_dimension.0 as u32,
        polynomial_size.0 as u32,
        ks_level.0 as u32,
        ks_base_log.0 as u32,
        pbs_level.0 as u32,
        pbs_base_log.0 as u32,
        grouping_factor.0 as u32,
        num_blocks,
        message_modulus.0 as u32,
        carry_modulus.0 as u32,
        pbs_type as u32,
        true,
    );
    cuda_apply_bivariate_lut_kb_64(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        radix_lwe_output.as_mut_c_ptr(0),
        radix_lwe_input_1.as_c_ptr(0),
        radix_lwe_input_2.as_c_ptr(0),
        mem_ptr,
        keyswitch_key.ptr.as_ptr(),
        bootstrapping_key.ptr.as_ptr(),
        num_blocks,
        shift,
    );
    cleanup_cuda_apply_bivariate_lut_kb_64(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
    );
}

#[allow(clippy::too_many_arguments)]
/// # Safety
///
/// - [CudaStreams::synchronize] __must__ be called after this function as soon as synchronization
///   is required
pub unsafe fn unchecked_div_rem_integer_radix_kb_assign_async<T: UnsignedInteger, B: Numeric>(
    streams: &CudaStreams,
    quotient: &mut CudaVec<T>,
    remainder: &mut CudaVec<T>,
    numerator: &CudaVec<T>,
    divisor: &CudaVec<T>,
    is_signed: bool,
    bootstrapping_key: &CudaVec<B>,
    keyswitch_key: &CudaVec<T>,
    message_modulus: MessageModulus,
    carry_modulus: CarryModulus,
    glwe_dimension: GlweDimension,
    polynomial_size: PolynomialSize,
    big_lwe_dimension: LweDimension,
    small_lwe_dimension: LweDimension,
    ks_level: DecompositionLevelCount,
    ks_base_log: DecompositionBaseLog,
    pbs_level: DecompositionLevelCount,
    pbs_base_log: DecompositionBaseLog,
    num_blocks: u32,
    pbs_type: PBSType,
    grouping_factor: LweBskGroupingFactor,
) {
    let mut mem_ptr: *mut i8 = std::ptr::null_mut();
    scratch_cuda_integer_div_rem_radix_ciphertext_kb_64(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        is_signed,
        std::ptr::addr_of_mut!(mem_ptr),
        glwe_dimension.0 as u32,
        polynomial_size.0 as u32,
        big_lwe_dimension.0 as u32,
        small_lwe_dimension.0 as u32,
        ks_level.0 as u32,
        ks_base_log.0 as u32,
        pbs_level.0 as u32,
        pbs_base_log.0 as u32,
        grouping_factor.0 as u32,
        num_blocks,
        message_modulus.0 as u32,
        carry_modulus.0 as u32,
        pbs_type as u32,
        true,
    );
    cuda_integer_div_rem_radix_ciphertext_kb_64(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        quotient.as_mut_c_ptr(0),
        remainder.as_mut_c_ptr(0),
        numerator.as_c_ptr(0),
        divisor.as_c_ptr(0),
        is_signed,
        mem_ptr,
        bootstrapping_key.ptr.as_ptr(),
        keyswitch_key.ptr.as_ptr(),
        num_blocks,
    );
    cleanup_cuda_integer_div_rem(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
    );
}

#[allow(clippy::too_many_arguments)]
/// # Safety
///
/// - [CudaStreams::synchronize] __must__ be called after this function as soon as synchronization
///   is required
pub unsafe fn compute_prefix_sum_hillis_steele_async<T: UnsignedInteger, B: Numeric>(
    streams: &CudaStreams,
    radix_lwe_output: &mut CudaSliceMut<T>,
    generates_or_propagates: &mut CudaSliceMut<T>,
    input_lut: &[T],
    bootstrapping_key: &CudaVec<B>,
    keyswitch_key: &CudaVec<T>,
    lwe_dimension: LweDimension,
    glwe_dimension: GlweDimension,
    polynomial_size: PolynomialSize,
    ks_level: DecompositionLevelCount,
    ks_base_log: DecompositionBaseLog,
    pbs_level: DecompositionLevelCount,
    pbs_base_log: DecompositionBaseLog,
    num_blocks: u32,
    message_modulus: MessageModulus,
    carry_modulus: CarryModulus,
    pbs_type: PBSType,
    grouping_factor: LweBskGroupingFactor,
    shift: u32,
) {
    assert_eq!(
        streams.gpu_indexes[0],
        generates_or_propagates.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        radix_lwe_output.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        bootstrapping_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        keyswitch_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    let mut mem_ptr: *mut i8 = std::ptr::null_mut();
    scratch_cuda_integer_compute_prefix_sum_hillis_steele_64(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
        input_lut.as_ptr().cast(),
        lwe_dimension.0 as u32,
        glwe_dimension.0 as u32,
        polynomial_size.0 as u32,
        ks_level.0 as u32,
        ks_base_log.0 as u32,
        pbs_level.0 as u32,
        pbs_base_log.0 as u32,
        grouping_factor.0 as u32,
        num_blocks,
        message_modulus.0 as u32,
        carry_modulus.0 as u32,
        pbs_type as u32,
        true,
    );

    cuda_integer_compute_prefix_sum_hillis_steele_64(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        radix_lwe_output.as_mut_c_ptr(0),
        generates_or_propagates.as_mut_c_ptr(0),
        mem_ptr,
        keyswitch_key.ptr.as_ptr(),
        bootstrapping_key.ptr.as_ptr(),
        num_blocks,
        shift,
    );

    cleanup_cuda_integer_compute_prefix_sum_hillis_steele_64(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
    );
}

#[allow(clippy::too_many_arguments)]
/// # Safety
///
/// - [CudaStreams::synchronize] __must__ be called after this function as soon as synchronization
///   is required
pub unsafe fn reverse_blocks_inplace_async<T: UnsignedInteger>(
    streams: &CudaStreams,
    radix_lwe_output: &mut CudaSliceMut<T>,
    num_blocks: u32,
    lwe_size: u32,
) {
    assert_eq!(
        streams.gpu_indexes[0],
        radix_lwe_output.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    if num_blocks > 1 {
        cuda_integer_reverse_blocks_64_inplace(
            streams.ptr.as_ptr(),
            streams
                .gpu_indexes
                .iter()
                .map(|i| i.0)
                .collect::<Vec<u32>>()
                .as_ptr(),
            streams.len() as u32,
            radix_lwe_output.as_mut_c_ptr(0),
            num_blocks,
            lwe_size,
        );
    }
}

#[allow(clippy::too_many_arguments)]
/// # Safety
///
/// - [CudaStreams::synchronize] __must__ be called after this function as soon as synchronization
///   is required
pub(crate) unsafe fn unchecked_unsigned_overflowing_sub_integer_radix_kb_assign_async<
    T: UnsignedInteger,
    B: Numeric,
>(
    streams: &CudaStreams,
    radix_lwe_input: &mut CudaVec<T>,
    radix_rhs_input: &CudaVec<T>,
    carry_out: &mut CudaVec<T>,
    carry_in: &CudaVec<T>,
    bootstrapping_key: &CudaVec<B>,
    keyswitch_key: &CudaVec<T>,
    lwe_dimension: LweDimension,
    glwe_dimension: GlweDimension,
    polynomial_size: PolynomialSize,
    ks_level: DecompositionLevelCount,
    ks_base_log: DecompositionBaseLog,
    pbs_level: DecompositionLevelCount,
    pbs_base_log: DecompositionBaseLog,
    num_blocks: u32,
    message_modulus: MessageModulus,
    carry_modulus: CarryModulus,
    pbs_type: PBSType,
    grouping_factor: LweBskGroupingFactor,
    compute_overflow: bool,
    uses_input_borrow: u32,
) {
    assert_eq!(
        streams.gpu_indexes[0],
        radix_lwe_input.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        bootstrapping_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        keyswitch_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    let mut mem_ptr: *mut i8 = std::ptr::null_mut();
    let big_lwe_dimension: u32 = glwe_dimension.0 as u32 * polynomial_size.0 as u32;
    scratch_cuda_integer_overflowing_sub_kb_64_inplace(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
        glwe_dimension.0 as u32,
        polynomial_size.0 as u32,
        big_lwe_dimension,
        lwe_dimension.0 as u32,
        ks_level.0 as u32,
        ks_base_log.0 as u32,
        pbs_level.0 as u32,
        pbs_base_log.0 as u32,
        grouping_factor.0 as u32,
        num_blocks,
        message_modulus.0 as u32,
        carry_modulus.0 as u32,
        pbs_type as u32,
        compute_overflow as u32,
        true,
    );
    cuda_integer_overflowing_sub_kb_64_inplace(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        radix_lwe_input.as_mut_c_ptr(0),
        radix_rhs_input.as_c_ptr(0),
        carry_out.as_mut_c_ptr(0),
        carry_in.as_c_ptr(0),
        mem_ptr,
        bootstrapping_key.ptr.as_ptr(),
        keyswitch_key.ptr.as_ptr(),
        num_blocks,
        compute_overflow as u32,
        uses_input_borrow,
    );
    cleanup_cuda_integer_overflowing_sub(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
    );
}

#[allow(clippy::too_many_arguments)]
/// # Safety
///
/// - [CudaStreams::synchronize] __must__ be called after this function as soon as synchronization
///   is required
pub unsafe fn unchecked_signed_abs_radix_kb_assign_async<T: UnsignedInteger, B: Numeric>(
    streams: &CudaStreams,
    ct: &mut CudaVec<T>,
    bootstrapping_key: &CudaVec<B>,
    keyswitch_key: &CudaVec<T>,
    message_modulus: MessageModulus,
    carry_modulus: CarryModulus,
    glwe_dimension: GlweDimension,
    polynomial_size: PolynomialSize,
    big_lwe_dimension: LweDimension,
    small_lwe_dimension: LweDimension,
    ks_level: DecompositionLevelCount,
    ks_base_log: DecompositionBaseLog,
    pbs_level: DecompositionLevelCount,
    pbs_base_log: DecompositionBaseLog,
    num_blocks: u32,
    pbs_type: PBSType,
    grouping_factor: LweBskGroupingFactor,
) {
    let mut mem_ptr: *mut i8 = std::ptr::null_mut();
    scratch_cuda_integer_abs_inplace_radix_ciphertext_kb_64(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
        true,
        glwe_dimension.0 as u32,
        polynomial_size.0 as u32,
        big_lwe_dimension.0 as u32,
        small_lwe_dimension.0 as u32,
        ks_level.0 as u32,
        ks_base_log.0 as u32,
        pbs_level.0 as u32,
        pbs_base_log.0 as u32,
        grouping_factor.0 as u32,
        num_blocks,
        message_modulus.0 as u32,
        carry_modulus.0 as u32,
        pbs_type as u32,
        true,
    );
    cuda_integer_abs_inplace_radix_ciphertext_kb_64(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        ct.as_mut_c_ptr(0),
        mem_ptr,
        true,
        bootstrapping_key.ptr.as_ptr(),
        keyswitch_key.ptr.as_ptr(),
        num_blocks,
    );
    cleanup_cuda_integer_abs_inplace(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
    );
}

#[allow(clippy::too_many_arguments)]
/// # Safety
///
/// - [CudaStreams::synchronize] __must__ be called after this function as soon as synchronization
///   is required
pub unsafe fn unchecked_is_at_least_one_comparisons_block_true_integer_radix_kb_async<
    T: UnsignedInteger,
    B: Numeric,
>(
    streams: &CudaStreams,
    radix_lwe_out: &mut CudaVec<T>,
    radix_lwe_in: &CudaVec<T>,
    bootstrapping_key: &CudaVec<B>,
    keyswitch_key: &CudaVec<T>,
    message_modulus: MessageModulus,
    carry_modulus: CarryModulus,
    glwe_dimension: GlweDimension,
    polynomial_size: PolynomialSize,
    big_lwe_dimension: LweDimension,
    small_lwe_dimension: LweDimension,
    ks_level: DecompositionLevelCount,
    ks_base_log: DecompositionBaseLog,
    pbs_level: DecompositionLevelCount,
    pbs_base_log: DecompositionBaseLog,
    num_blocks: u32,
    pbs_type: PBSType,
    grouping_factor: LweBskGroupingFactor,
) {
    assert_eq!(
        streams.gpu_indexes[0],
        radix_lwe_out.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        radix_lwe_in.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        bootstrapping_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        keyswitch_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    let mut mem_ptr: *mut i8 = std::ptr::null_mut();
    scratch_cuda_integer_is_at_least_one_comparisons_block_true_kb_64(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
        glwe_dimension.0 as u32,
        polynomial_size.0 as u32,
        big_lwe_dimension.0 as u32,
        small_lwe_dimension.0 as u32,
        ks_level.0 as u32,
        ks_base_log.0 as u32,
        pbs_level.0 as u32,
        pbs_base_log.0 as u32,
        grouping_factor.0 as u32,
        num_blocks,
        message_modulus.0 as u32,
        carry_modulus.0 as u32,
        pbs_type as u32,
        true,
    );

    cuda_integer_is_at_least_one_comparisons_block_true_kb_64(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        radix_lwe_out.as_mut_c_ptr(0),
        radix_lwe_in.as_c_ptr(0),
        mem_ptr,
        bootstrapping_key.ptr.as_ptr(),
        keyswitch_key.ptr.as_ptr(),
        num_blocks,
    );

    cleanup_cuda_integer_is_at_least_one_comparisons_block_true(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
    );
}

#[allow(clippy::too_many_arguments)]
/// # Safety
///
/// - [CudaStreams::synchronize] __must__ be called after this function as soon as synchronization
///   is required
pub unsafe fn unchecked_are_all_comparisons_block_true_integer_radix_kb_async<
    T: UnsignedInteger,
    B: Numeric,
>(
    streams: &CudaStreams,
    radix_lwe_out: &mut CudaVec<T>,
    radix_lwe_in: &CudaVec<T>,
    bootstrapping_key: &CudaVec<B>,
    keyswitch_key: &CudaVec<T>,
    message_modulus: MessageModulus,
    carry_modulus: CarryModulus,
    glwe_dimension: GlweDimension,
    polynomial_size: PolynomialSize,
    big_lwe_dimension: LweDimension,
    small_lwe_dimension: LweDimension,
    ks_level: DecompositionLevelCount,
    ks_base_log: DecompositionBaseLog,
    pbs_level: DecompositionLevelCount,
    pbs_base_log: DecompositionBaseLog,
    num_blocks: u32,
    pbs_type: PBSType,
    grouping_factor: LweBskGroupingFactor,
) {
    assert_eq!(
        streams.gpu_indexes[0],
        radix_lwe_out.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        radix_lwe_in.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        bootstrapping_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    assert_eq!(
        streams.gpu_indexes[0],
        keyswitch_key.gpu_index(0),
        "GPU error: all data should reside on the same GPU."
    );
    let mut mem_ptr: *mut i8 = std::ptr::null_mut();
    scratch_cuda_integer_are_all_comparisons_block_true_kb_64(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
        glwe_dimension.0 as u32,
        polynomial_size.0 as u32,
        big_lwe_dimension.0 as u32,
        small_lwe_dimension.0 as u32,
        ks_level.0 as u32,
        ks_base_log.0 as u32,
        pbs_level.0 as u32,
        pbs_base_log.0 as u32,
        grouping_factor.0 as u32,
        num_blocks,
        message_modulus.0 as u32,
        carry_modulus.0 as u32,
        pbs_type as u32,
        true,
    );

    cuda_integer_are_all_comparisons_block_true_kb_64(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        radix_lwe_out.as_mut_c_ptr(0),
        radix_lwe_in.as_c_ptr(0),
        mem_ptr,
        bootstrapping_key.ptr.as_ptr(),
        keyswitch_key.ptr.as_ptr(),
        num_blocks,
    );

    cleanup_cuda_integer_are_all_comparisons_block_true(
        streams.ptr.as_ptr(),
        streams
            .gpu_indexes
            .iter()
            .map(|i| i.0)
            .collect::<Vec<u32>>()
            .as_ptr(),
        streams.len() as u32,
        std::ptr::addr_of_mut!(mem_ptr),
    );
}
