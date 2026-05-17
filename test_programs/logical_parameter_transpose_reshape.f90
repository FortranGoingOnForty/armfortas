! CHECK: ok
! IR_CHECK: __prog_logical_parameter_transpose_reshape
! REPRO_CHECK: run
program logical_parameter_transpose_reshape
  implicit none
  logical :: arr(2, 3)
  logical, parameter :: expected(2, 3) = reshape([ &
      .true., .true., &
      .false., .true., &
      .false., .false. &
    ], shape=[2, 3])
  logical, parameter :: got(2, 3) = transpose(reshape([ &
      .true., .false., .false., &
      .true., .true., .false. &
    ], shape=[3, 2]))

  arr = expected

  if (.not. all(arr .eqv. got)) error stop 1
  if (.not. got(1, 1)) error stop 2
  if (.not. got(2, 1)) error stop 3
  if (got(1, 2)) error stop 4
  if (.not. got(2, 2)) error stop 5
  if (got(1, 3)) error stop 6
  if (got(2, 3)) error stop 7

  print *, 'ok'
end program
