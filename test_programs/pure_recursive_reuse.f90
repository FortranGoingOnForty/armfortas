! Repeated PURE recursive calls should be reused at O2+ in the caller.
! IR_CHECK: call @afs_internal___prog_pure_recursive_reuse_1(
! IR_CHECK: call @afs_internal___prog_pure_recursive_reuse_1(
! CHECK: 1440
program pure_recursive_reuse
  implicit none
  integer :: first
  integer :: second

  first = heavy_fact(6)
  second = heavy_fact(6)
  print *, first + second
contains
  recursive pure function heavy_fact(n) result(r)
    integer, intent(in) :: n
    integer :: r

    if (n <= 1) then
      r = 1
    else
      r = n * heavy_fact(n - 1)
    end if
  end function
end program
