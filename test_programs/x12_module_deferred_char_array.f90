! Regression: a module/host-associated deferred-length allocatable
! CHARACTER ARRAY must allocate and index as an array, not a scalar
! string. The module global recorded char_kind=Deferred, so the
! ALLOCATE statement took the 32-byte scalar-string path: it allocated
! one element's worth of bytes and read the descriptor as a string, so
! SIZE was 0 and element reads returned single bytes. A local
! `character(len=:), allocatable :: x(:)` records char_kind=None and
! carries the element length in the descriptor's elem_size; the module
! path now matches. Surfaced building fpm's vendored M_CLI2 keywords.
! x12.
!
! CHECK: n=2
! CHECK: len=8
! CHECK: a=alpha
! CHECK: b=beta
module m
  implicit none
  character(len=:), allocatable, save :: keywords(:)
contains
  subroutine doit()
    allocate(character(len=8) :: keywords(2))
    keywords(1) = 'alpha'
    keywords(2) = 'beta'
    write(*, '(a,i0)') 'n=', size(keywords)
    write(*, '(a,i0)') 'len=', len(keywords)
    write(*, '(a,a)') 'a=', trim(keywords(1))
    write(*, '(a,a)') 'b=', trim(keywords(2))
  end subroutine
end module
program t
  use m
  call doit()
end program
