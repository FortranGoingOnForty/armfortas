! Unused PURE calls should disappear once the optimized pipeline proves the
! result is dead.
! CHECK: 42
program pure_dead_call
  implicit none
  integer :: sink

  sink = heavy_fact(6)
  print *, 42

contains

  recursive pure function heavy_fact(n) result(r)
    integer, intent(in) :: n
    integer :: r

    if (n <= 1) then
      r = 1
    else
      r = n * heavy_fact(n - 1)
    end if
  end function heavy_fact

end program pure_dead_call
