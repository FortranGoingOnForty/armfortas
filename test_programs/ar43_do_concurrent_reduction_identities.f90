! REDUCE construct entities start from the operator identity and combine with
! the outside accumulator when the construct terminates. Integer identities
! retain the reduction variable's kind, MAX uses the true least representable
! value, and pointer outside variables get nonpointer scalar/array construct
! entities that define their associated targets without replacing association.
! Character MAX/MIN use the ends of the processor's byte collating sequence.
!
! FLAGS: --std=f2023
! CHECK: 48 1 0 7 4 T T
! CHECK: T T
! CHECK: -128 -9223372036854775808 -9223372036854775808 -9223372036854775808
! CHECK: 103 206 13 26
! CHECK: 65 90 65 90
! CHECK: 4 -2 2 6
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
program ar43_do_concurrent_reduction_identities
  use iso_fortran_env, only : int8, int64, real64
  implicit none
  integer :: i
  integer :: product, minimum, intersection, union, parity
  integer(int8) :: maximum8
  integer(int64), target :: pointer_target64
  integer(int64), pointer :: pointer64
  integer(int64), target :: pointer_array_target(2)
  integer(int64), pointer :: pointer_array(:)
  integer(int64) :: vector_sum(2)
  integer(int64) :: maximum64, intersection64
  logical :: any_even, all_small, equivalent, parity_logical
  character(len=2) :: maximum_char, minimum_char
  complex(real64) :: complex_sum, complex_product

  product = 2
  minimum = 100
  intersection = -1
  union = 0
  parity = 0
  any_even = .false.
  all_small = .true.
  equivalent = .true.
  parity_logical = .false.
  maximum8 = -huge(maximum8) - 1_int8
  maximum64 = -huge(maximum64) - 1_int64
  intersection64 = -1_int64
  pointer_target64 = -1_int64
  pointer64 => pointer_target64
  pointer_array_target = [10_int64, 20_int64]
  pointer_array => pointer_array_target
  vector_sum = [100_int64, 200_int64]
  maximum_char = achar(0) // achar(0)
  minimum_char = char(255) // char(255)
  complex_sum = cmplx(1.0_real64, 1.0_real64, kind=real64)
  complex_product = cmplx(2.0_real64, 0.0_real64, kind=real64)

  do concurrent (i = 1:4) reduce(*:product) reduce(min:minimum) &
      reduce(iand:intersection) reduce(ior:union) reduce(ieor:parity) &
      reduce(.or.:any_even) reduce(.and.:all_small) &
      reduce(.eqv.:equivalent) reduce(.neqv.:parity_logical)
    product = product * i
    minimum = min(minimum, i)
    intersection = iand(intersection, 7 - i)
    union = ior(union, i)
    parity = ieor(parity, i)
    any_even = any_even .or. mod(i, 2) == 0
    all_small = all_small .and. i < 5
    equivalent = equivalent .eqv. i < 5
    parity_logical = parity_logical .neqv. i == 2
  end do

  do concurrent (i = 1:1) reduce(max:maximum8) reduce(max:maximum64) &
      reduce(iand:intersection64) reduce(iand:pointer64)
    maximum8 = max(maximum8, -huge(maximum8) - 1_int8)
    maximum64 = max(maximum64, -huge(maximum64) - 1_int64)
    intersection64 = iand(intersection64, -huge(intersection64) - 1_int64)
    pointer64 = iand(pointer64, -huge(pointer64) - 1_int64)
  end do

  do concurrent (i = 1:2) reduce(+:vector_sum) reduce(+:pointer_array)
    vector_sum = vector_sum + [int(i, int64), int(2 * i, int64)]
    pointer_array = pointer_array + [int(i, int64), int(2 * i, int64)]
  end do

  do concurrent (i = 1:1) reduce(max:maximum_char) reduce(min:minimum_char)
    maximum_char = max(maximum_char, 'AZ')
    minimum_char = min(minimum_char, 'AZ')
  end do

  do concurrent (i = 1:2) reduce(+:complex_sum) reduce(*:complex_product)
    complex_sum = complex_sum + cmplx(real(i, real64), -real(i, real64), kind=real64)
    complex_product = complex_product * cmplx(real(i, real64), 1.0_real64, kind=real64)
  end do

  print *, product, minimum, intersection, union, parity, any_even, all_small
  print *, equivalent, parity_logical
  print *, maximum8, maximum64, intersection64, pointer_target64
  print *, vector_sum, pointer_array_target
  print *, iachar(maximum_char(1:1)), iachar(maximum_char(2:2)), &
      iachar(minimum_char(1:1)), iachar(minimum_char(2:2))
  print *, nint(real(complex_sum)), nint(aimag(complex_sum)), &
      nint(real(complex_product)), nint(aimag(complex_product))
  if (product /= 48) error stop 1
  if (minimum /= 1) error stop 2
  if (intersection /= 0) error stop 3
  if (union /= 7) error stop 4
  if (parity /= 4) error stop 5
  if (.not. any_even) error stop 6
  if (.not. all_small) error stop 7
  if (.not. equivalent) error stop 16
  if (.not. parity_logical) error stop 17
  if (maximum8 /= -huge(maximum8) - 1_int8) error stop 8
  if (maximum64 /= -huge(maximum64) - 1_int64) error stop 9
  if (intersection64 /= -huge(intersection64) - 1_int64) error stop 10
  if (pointer_target64 /= -huge(pointer_target64) - 1_int64) error stop 11
  if (any(vector_sum /= [103_int64, 206_int64])) error stop 12
  if (any(pointer_array_target /= [13_int64, 26_int64])) error stop 13
  if (maximum_char /= 'AZ') error stop 14
  if (minimum_char /= 'AZ') error stop 15
  if (complex_sum /= cmplx(4.0_real64, -2.0_real64, kind=real64)) error stop 18
  if (complex_product /= cmplx(2.0_real64, 6.0_real64, kind=real64)) error stop 19
end program ar43_do_concurrent_reduction_identities
