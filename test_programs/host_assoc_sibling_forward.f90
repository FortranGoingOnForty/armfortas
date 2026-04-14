! Sibling forwarding: contained procedure `a` calls contained `b`,
! and both access the same host local `x`. `a` receives `x`'s host
! address as a hidden param; when it calls `b`, it must load that
! spill slot and forward the original host address (not the address
! of its own spill). Proves append_host_closure_args's by_ref path.
! CHECK: 100
program sibling_forward
  implicit none
  integer :: x
  x = 0
  call a()
  print *, x
contains
  subroutine a()
    x = x + 50
    call b()
  end subroutine
  subroutine b()
    x = x + 50
  end subroutine
end program
