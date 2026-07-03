! Regression (fpm fpm_versioning::next hang): the descriptor-parameter mask
! is cached in a map keyed by bare procedure name. When two modules define a
! procedure with the same bare name, the last-collected one's mask clobbers
! the others'. `descriptor_param_mask_for_lookup` then returned that wrong
! mask for a call that resolved to a *different* module's procedure.
!
! Here m_ver::tokenize has a character(len=*) first arg and an allocatable
! derived `error` last arg (descriptor mask [F,F,F,T]). m_lex::tokenize —
! defined last — takes an ALLOCATABLE derived first arg (mask [T,F]). Without
! the qualified-key fix, `parse`'s `call tokenize(string,...)` used [T,F]:
! arg0 (string) was wrongly marshalled as a descriptor and received a null
! data pointer, so `len(string)`/substring reads faulted or looped forever.
module m_ver
  implicit none
  type :: error_t
     character(len=:), allocatable :: message
  end type
contains
  subroutine tokenize(string, istart, iend, error)
     character(len=*), intent(in) :: string
     integer, intent(inout) :: istart, iend
     type(error_t), allocatable, intent(out) :: error
     integer :: nn
     nn = len(string)
     if (iend >= nn) then
        istart = nn; iend = nn; return
     end if
     istart = iend + 1
     iend = min(iend + 2, nn)
     if (string(istart:istart) == '!') then
        allocate(error)
        error%message = 'bang'
     end if
  end subroutine

  subroutine parse(string, ntok)
     character(len=*), intent(in) :: string
     integer, intent(out) :: ntok
     integer :: istart, iend
     type(error_t), allocatable :: error
     istart = 0; iend = 0; ntok = 0
     do while (iend < len(string))
        call tokenize(string, istart, iend, error)
        if (allocated(error)) exit
        ntok = ntok + 1
        if (ntok > 100) exit   ! guard: a miscompile that loops fails fast
     end do
  end subroutine
end module

module m_lex
  implicit none
  type :: node_t
     integer :: kind = 0
  end type
contains
  ! Same bare name `tokenize`, defined AFTER m_ver, first arg descriptor-passed.
  subroutine tokenize(node, count)
     type(node_t), allocatable, intent(inout) :: node
     integer, intent(out) :: count
     if (.not. allocated(node)) allocate(node)
     node%kind = node%kind + 1
     count = node%kind
  end subroutine
end module

program p
  use m_ver, only: parse
  implicit none
  integer :: ntok
  call parse('ab.cd.ef', ntok)
  write(*, '(a,i0)') 'ntok=', ntok
  if (ntok /= 4) error stop 1
end program p
! CHECK: ntok=4
