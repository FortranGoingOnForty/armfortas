! Complex POINTER read and print: `pc => c` stores the address of
! the target complex buffer into the pointer slot, and reading `pc`
! in a value context loads once to recover that buffer address.
! Previously the scalar-pointer path tried to load the complex as
! a scalar value of its aggregate type, which tripped the coercion
! helper and verifier (Ptr(Array(f32,2)) → Array(f32,2) was unhandled).
! CHECK: 1.5000000E0
program t
  implicit none
  complex, target :: c = (1.5, 2.5)
  complex, pointer :: pc
  pc => c
  print *, pc
end program
