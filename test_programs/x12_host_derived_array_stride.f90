! Regression: indexing a HOST-ASSOCIATED derived-type array from an
! internal procedure must stride by the derived type's real byte size.
! armfortas's host-association closure ABI typed the element as the
! lower_type_spec fallback for derived types (Ptr<i8>, 8 bytes), so the
! contained proc's array indexing scaled the element offset by 8 instead
! of the struct size. Element 1 (offset 0) was fine; element k>1 of a
! host-associated derived array landed at the wrong address -> wrong data
! or SIGSEGV. The bug only bit derived types bigger than 8 bytes; a type
! with an allocatable component (a 32-byte string descriptor) is the
! common case. Surfaced building fortsh: pipeline_helpers grow_temp_arrays
! grows a host `string_t` token array from a contained subprogram, and
! word-splitting `echo $(seq 1 500)` truncated/crashed once the array
! exceeded its initial 256 capacity. The fix resolves the host-ref
! element type via the type-layout registry, like a normal dummy. x12.
!
! CHECK: k=1 a one
! CHECK: k=2 b two
! CHECK: k=3 c three
! CHECK: k=4 d four
! CHECK: all 4 ok
module x12_hdas
  implicit none
  type :: kv
    character(:), allocatable :: key   ! 32-byte string descriptor component
    integer :: n = 0
  end type
contains
  subroutine run()
    type(kv), allocatable :: a(:)
    integer :: i, ok
    allocate(a(4))
    a(1)%key = 'a'; a(1)%n = 1
    a(2)%key = 'b'; a(2)%n = 2
    a(3)%key = 'c'; a(3)%n = 3
    a(4)%key = 'd'; a(4)%n = 4
    ! show() and count_ok() are SIBLING internal procs that index the
    ! host-associated derived array a(:) at k > 1.
    call show()
    ok = count_ok()
    write(*, '(a,i0,a)') 'all ', ok, ' ok'
  contains
    subroutine show()
      integer :: k
      character(len=8) :: word
      do k = 1, 4
        select case (a(k)%n)
        case (1); word = 'one'
        case (2); word = 'two'
        case (3); word = 'three'
        case (4); word = 'four'
        case default; word = '??'
        end select
        write(*, '(a,i0,a,a,a,a)') 'k=', k, ' ', trim(a(k)%key), ' ', trim(word)
      end do
    end subroutine
    integer function count_ok() result(c)
      integer :: k
      c = 0
      do k = 1, 4
        if (allocated(a(k)%key) .and. a(k)%n == k) c = c + 1
      end do
    end function
  end subroutine
end module

program main
  use x12_hdas
  implicit none
  call run()
end program
