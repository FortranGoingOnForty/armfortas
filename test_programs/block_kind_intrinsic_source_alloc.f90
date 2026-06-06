! CHECK: sp
! CHECK: dp
! CHECK: ok
! PHASE_TRIANGULATE: ir|asm|obj|repro
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2 => stdout|exit
program p
  implicit none

  block
    integer, parameter :: wp = 4
    real(wp), allocatable :: dense(:,:)
    allocate(dense(2,2), source=reshape(real([1,2,3,4], kind=wp), [2,2]))
    print *, 'sp', dense(1,1), sum(dense)
  end block

  block
    integer, parameter :: wp = 8
    real(wp), allocatable :: dense(:,:)
    allocate(dense(2,2), source=reshape(real([5,6,7,8], kind=wp), [2,2]))
    print *, 'dp', dense(1,1), sum(dense)
    if (abs(dense(1,1) - 5.0_8) > 0.001_8) error stop 1
  end block

  print *, 'ok'
end program
