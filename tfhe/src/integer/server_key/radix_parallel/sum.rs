use crate::integer::ciphertext::IntegerRadixCiphertext;
use crate::integer::{BooleanBlock, IntegerCiphertext, RadixCiphertext, ServerKey};
use crate::shortint::ciphertext::Degree;
use crate::shortint::Ciphertext;
use rayon::prelude::*;

impl ServerKey {
    /// Computes the sum of the ciphertexts in parallel.
    ///
    /// output_carries: if not None, carries generated by last blocks will
    /// be stored in it.
    ///
    /// Returns a result that has non propagated carries
    pub(crate) fn unchecked_partial_sum_ciphertexts_vec_parallelized<T>(
        &self,
        terms: Vec<T>,
        mut output_carries: Option<&mut Vec<Ciphertext>>,
    ) -> Option<T>
    where
        T: IntegerRadixCiphertext,
    {
        if terms.is_empty() {
            return None;
        }

        if terms.len() == 1 {
            return Some(terms.into_iter().next().unwrap());
        }

        let num_blocks = terms[0].blocks().len();
        assert!(
            terms[1..].iter().all(|ct| ct.blocks().len() == num_blocks),
            "Not all ciphertexts have the same number of blocks"
        );

        if terms.len() == 2 {
            return Some(self.add_parallelized(&terms[0], &terms[1]));
        }

        assert!(
            terms
                .iter()
                .all(IntegerRadixCiphertext::block_carries_are_empty),
            "All ciphertexts must have empty carries"
        );

        // Pre-conditions and easy path are met, start the real work
        let num_elements_to_fill_carry =
            self.max_sum_size(Degree::new(self.key.message_modulus.0 - 1));

        // Re-organize radix terms into columns of blocks
        let mut columns = vec![vec![]; num_blocks];
        for term in terms {
            for (i, block) in term.into_blocks().into_iter().enumerate() {
                if block.degree.get() != 0 {
                    columns[i].push(block);
                }
            }
        }

        if columns.iter().all(Vec::is_empty) {
            return Some(self.create_trivial_radix(0, num_blocks));
        }

        let num_columns = columns.len();
        // Buffer in which we will store resulting columns after an iteration
        let mut columns_buffer = Vec::with_capacity(num_columns);
        let mut column_output_buffer =
            vec![Vec::<(Ciphertext, Option<Ciphertext>)>::new(); num_blocks];

        let at_least_one_column_has_enough_elements = |columns: &[Vec<Ciphertext>]| {
            columns.iter().any(|c| c.len() > num_elements_to_fill_carry)
        };

        while at_least_one_column_has_enough_elements(&columns) {
            columns
                .par_drain(..)
                .zip(column_output_buffer.par_iter_mut())
                .enumerate()
                .map(|(column_index, (mut column, out_buf))| {
                    if column.len() < num_elements_to_fill_carry {
                        return column;
                    }
                    column
                        .par_chunks_exact(num_elements_to_fill_carry)
                        .map(|chunk| {
                            let mut result = chunk[0].clone();
                            for c in &chunk[1..] {
                                self.key.unchecked_add_assign(&mut result, c);
                            }

                            if (column_index < num_columns - 1) || output_carries.is_some() {
                                rayon::join(
                                    || self.key.message_extract(&result),
                                    || Some(self.key.carry_extract(&result)),
                                )
                            } else {
                                (self.key.message_extract(&result), None)
                            }
                        })
                        .collect_into_vec(out_buf);

                    let num_elem_in_rest = column.len() % num_elements_to_fill_carry;
                    column.rotate_right(num_elem_in_rest);
                    column.truncate(num_elem_in_rest);
                    column
                })
                .collect_into_vec(&mut columns_buffer);

            std::mem::swap(&mut columns, &mut columns_buffer);

            // Move resulting message and carry blocks where they belong
            for (i, column_output) in column_output_buffer.iter_mut().enumerate() {
                for (msg, maybe_carry) in column_output.drain(..) {
                    columns[i].push(msg);

                    if let Some(carry) = maybe_carry {
                        if (i + 1) < columns.len() {
                            columns[i + 1].push(carry);
                        } else if let Some(ref mut out) = output_carries {
                            out.push(carry);
                        }
                    }
                }
            }
        }

        // Reconstruct a radix from the columns
        let blocks = columns
            .into_iter()
            .map(|mut column| {
                if column.is_empty() {
                    self.key.create_trivial(0)
                } else {
                    let (first_block, other_blocks) =
                        column.as_mut_slice().split_first_mut().unwrap();
                    for other in other_blocks {
                        self.key.unchecked_add_assign(first_block, other);
                    }
                    column.swap_remove(0)
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(blocks.len(), num_blocks);

        Some(T::from_blocks(blocks))
    }

    /// Computes the sum of the ciphertexts in parallel.
    ///
    /// - Returns None if ciphertexts is empty
    ///
    /// - Expects all ciphertexts to have empty carries
    /// - Expects all ciphertexts to have the same size
    pub fn unchecked_sum_ciphertexts_vec_parallelized<T>(&self, ciphertexts: Vec<T>) -> Option<T>
    where
        T: IntegerRadixCiphertext,
    {
        let mut result =
            self.unchecked_partial_sum_ciphertexts_vec_parallelized(ciphertexts, None)?;

        self.full_propagate_parallelized(&mut result);
        assert!(result.block_carries_are_empty());

        Some(result)
    }

    /// See [Self::unchecked_sum_ciphertexts_vec_parallelized]
    pub fn unchecked_sum_ciphertexts_parallelized<'a, T, C>(&self, ciphertexts: C) -> Option<T>
    where
        C: IntoIterator<Item = &'a T>,
        T: IntegerRadixCiphertext + 'a,
    {
        let ciphertexts = ciphertexts.into_iter().map(Clone::clone).collect();
        self.unchecked_sum_ciphertexts_vec_parallelized(ciphertexts)
    }

    /// Computes the sum of the ciphertexts in parallel.
    ///
    /// - Returns None if ciphertexts is empty
    ///
    /// See [Self::unchecked_sum_ciphertexts_parallelized] for constraints
    pub fn sum_ciphertexts_parallelized<'a, T, C>(&self, ciphertexts: C) -> Option<T>
    where
        C: IntoIterator<Item = &'a T>,
        T: IntegerRadixCiphertext + 'a,
    {
        let mut ciphertexts = ciphertexts
            .into_iter()
            .map(Clone::clone)
            .collect::<Vec<T>>();
        ciphertexts
            .par_iter_mut()
            .filter(|ct| ct.block_carries_are_empty())
            .for_each(|ct| {
                if !ct.block_carries_are_empty() {
                    self.full_propagate_parallelized(&mut *ct);
                }
            });

        self.unchecked_sum_ciphertexts_vec_parallelized(ciphertexts)
    }

    /// Computes the sum of the ciphertexts in parallel.
    ///
    /// - Returns None if ciphertexts is empty
    ///
    /// See [Self::unchecked_sum_ciphertexts_parallelized] for constraints
    pub fn smart_sum_ciphertexts_parallelized<T, C>(&self, mut ciphertexts: C) -> Option<T>
    where
        C: AsMut<[T]> + AsRef<[T]>,
        T: IntegerRadixCiphertext,
    {
        ciphertexts.as_mut().par_iter_mut().for_each(|ct| {
            if !ct.block_carries_are_empty() {
                self.full_propagate_parallelized(ct);
            }
        });

        self.unchecked_sum_ciphertexts_parallelized(ciphertexts.as_ref())
    }

    /// - Expects all ciphertexts to have empty carries
    /// - Expects all ciphertexts to have the same size
    pub fn unchecked_unsigned_overflowing_sum_ciphertexts_vec_parallelized(
        &self,
        mut ciphertexts: Vec<RadixCiphertext>,
    ) -> Option<(RadixCiphertext, BooleanBlock)> {
        if ciphertexts.is_empty() {
            return None;
        }

        if ciphertexts.len() == 1 {
            return Some((
                ciphertexts.pop().unwrap(),
                BooleanBlock::new_unchecked(self.key.create_trivial(0)),
            ));
        }

        let num_blocks = ciphertexts[0].blocks().len();
        assert!(
            ciphertexts[1..]
                .iter()
                .all(|ct| ct.blocks().len() == num_blocks),
            "Not all ciphertexts have the same number of blocks"
        );

        if ciphertexts.len() == 2 {
            return Some(
                self.unsigned_overflowing_add_parallelized(&ciphertexts[0], &ciphertexts[1]),
            );
        }

        let mut carries = Vec::with_capacity(15);
        let un_propagated_result = self
            .unchecked_partial_sum_ciphertexts_vec_parallelized(ciphertexts, Some(&mut carries))?;

        let (message_blocks, carry_blocks) = rayon::join(
            || {
                un_propagated_result
                    .blocks
                    .par_iter()
                    .map(|block| self.key.message_extract(block))
                    .collect::<Vec<_>>()
            },
            || {
                let mut carry_blocks = Vec::with_capacity(num_blocks);
                un_propagated_result
                    .blocks
                    .par_iter()
                    .map(|block| self.key.carry_extract(block))
                    .collect_into_vec(&mut carry_blocks);
                carries.push(carry_blocks.pop().unwrap());
                carry_blocks.insert(0, self.key.create_trivial(0));
                carry_blocks
            },
        );

        let ((result, overflowed), any_sum_overflowed) = rayon::join(
            || {
                let mut result = RadixCiphertext::from(message_blocks);
                let carry = RadixCiphertext::from(carry_blocks);
                let overflowed =
                    self.unsigned_overflowing_add_assign_parallelized(&mut result, &carry);
                assert!(result.block_carries_are_empty());
                (result, overflowed)
            },
            || {
                let mut carries = RadixCiphertext::from(carries);
                carries.blocks.retain(|block| block.degree.get() != 0);
                self.scalar_ne_parallelized(&carries, 0)
            },
        );

        let overflowed = self.boolean_bitor(&overflowed, &any_sum_overflowed);

        Some((result, overflowed))
    }

    /// Computes the sum of the unsigned ciphertexts in parallel.
    /// Returns a boolean indicating if the sum overflowed, that is,
    /// the result did not fit in a ciphertext.
    ///
    /// See [Self::unchecked_sum_ciphertexts_vec_parallelized]
    pub fn unchecked_unsigned_overflowing_sum_ciphertexts_parallelized<'a, C>(
        &self,
        ciphertexts: C,
    ) -> Option<(RadixCiphertext, BooleanBlock)>
    where
        C: IntoIterator<Item = &'a RadixCiphertext>,
    {
        let ciphertexts = ciphertexts.into_iter().map(Clone::clone).collect();
        self.unchecked_unsigned_overflowing_sum_ciphertexts_vec_parallelized(ciphertexts)
    }

    /// Computes the sum of the unsigned ciphertexts in parallel.
    /// Returns a boolean indicating if the sum overflowed, that is,
    /// the result did not fit in a ciphertext.
    ///
    /// - Returns None if ciphertexts is empty
    ///
    /// See [Self::unchecked_sum_ciphertexts_parallelized] for constraints
    pub fn unsigned_overflowing_sum_ciphertexts_parallelized<'a, C>(
        &self,
        ciphertexts: C,
    ) -> Option<(RadixCiphertext, BooleanBlock)>
    where
        C: IntoIterator<Item = &'a RadixCiphertext>,
    {
        let mut ciphertexts = ciphertexts
            .into_iter()
            .map(Clone::clone)
            .collect::<Vec<_>>();
        ciphertexts
            .par_iter_mut()
            .filter(|ct| ct.block_carries_are_empty())
            .for_each(|ct| {
                if !ct.block_carries_are_empty() {
                    self.full_propagate_parallelized(&mut *ct);
                }
            });

        self.unchecked_unsigned_overflowing_sum_ciphertexts_vec_parallelized(ciphertexts)
    }

    /// Computes the sum of the unsigned ciphertexts in parallel.
    /// Returns a boolean indicating if the sum overflowed, that is,
    /// the result did not fit in a ciphertext.
    ///
    /// - Returns None if ciphertexts is empty
    ///
    /// See [Self::unchecked_sum_ciphertexts_parallelized] for constraints
    pub fn smart_unsigned_overflowing_sum_ciphertexts_parallelized<C>(
        &self,
        mut ciphertexts: C,
    ) -> Option<(RadixCiphertext, BooleanBlock)>
    where
        C: AsMut<[RadixCiphertext]> + AsRef<[RadixCiphertext]>,
    {
        ciphertexts.as_mut().par_iter_mut().for_each(|ct| {
            if !ct.block_carries_are_empty() {
                self.full_propagate_parallelized(ct);
            }
        });

        self.unchecked_unsigned_overflowing_sum_ciphertexts_parallelized(ciphertexts.as_ref())
    }
}
