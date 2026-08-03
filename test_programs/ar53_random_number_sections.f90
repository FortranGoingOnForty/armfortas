! RANDOM_NUMBER must define exactly the elements selected by a HARVEST
! descriptor. These cases distinguish descriptor traversal from a raw
! contiguous write while keeping every pre-repair write inside backing storage.
!
! FLAGS: --std=f2018
! CHECK: ok
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
! IR_CHECK: call @afs_random_number_array_desc(
! IR_NOT: call @afs_random_number_array_f32(
! IR_NOT: call @afs_random_number_array_f64(
program ar53_random_number_sections
  use iso_fortran_env, only: real32, real64
  implicit none

  real(real32), parameter :: sentinel32 = -1.0_real32
  real(real64), parameter :: sentinel64 = -1.0_real64
  real(real32) :: forward(20)
  real(real64) :: reverse(24)
  real(real32) :: matrix(6, 5)
  real(real64) :: empty(4)
  integer :: i, j

  forward = sentinel32
  call random_number(forward(2:16:2))
  do i = 1, size(forward)
    if (i >= 2 .and. i <= 16 .and. mod(i, 2) == 0) then
      if (forward(i) < 0.0_real32 .or. forward(i) >= 1.0_real32) error stop 1
    else
      if (forward(i) /= sentinel32) error stop 2
    end if
  end do

  reverse = sentinel64
  call random_number(reverse(16:2:-2))
  do i = 1, size(reverse)
    if (i >= 2 .and. i <= 16 .and. mod(i, 2) == 0) then
      if (reverse(i) < 0.0_real64 .or. reverse(i) >= 1.0_real64) error stop 3
    else
      if (reverse(i) /= sentinel64) error stop 4
    end if
  end do

  matrix = sentinel32
  call random_number(matrix(2:6:2, 1:5:2))
  do j = 1, size(matrix, 2)
    do i = 1, size(matrix, 1)
      if (mod(i, 2) == 0 .and. mod(j, 2) == 1) then
        if (matrix(i, j) < 0.0_real32 .or. matrix(i, j) >= 1.0_real32) error stop 5
      else
        if (matrix(i, j) /= sentinel32) error stop 6
      end if
    end do
  end do

  empty = sentinel64
  call random_number(empty(2:1))
  if (any(empty /= sentinel64)) error stop 7

  print '(a)', 'ok'
end program ar53_random_number_sections
