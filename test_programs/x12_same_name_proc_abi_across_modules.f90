! Regression (fpm fpm_versioning::next): when two modules define a
! procedure with the same bare name but different signatures, a call must
! use the ABI (descriptor/character-length masks) of the procedure it
! actually resolves to, not whichever same-named one was collected last.
! armfortas keyed its per-callee ABI masks by bare name; the second
! module's `next` (non-character first arg) clobbered the first's, so
! `parse`'s `call next(string, ...)` marshalled the character(len=*)
! `string` with the wrong ABI and passed a null data pointer (infinite
! loop / crash). `m_second` is defined AFTER `m_first` so the wrong entry
! wins without the caller-scope-aware fix.
module m_first
  implicit none
  type :: acc_t
     integer, allocatable :: num(:)
  end type
contains
  subroutine step(string, istart, iend, is_num)
     character(len=*), intent(in) :: string
     integer, intent(inout) :: istart, iend
     logical, intent(inout) :: is_num
     integer :: ii, nn
     character :: tok
     nn = len(string)
     if (iend >= nn) then; istart = nn; iend = nn; return; end if
     ii = min(iend + 1, nn)
     tok = string(ii:ii)
     is_num = tok /= '.'
     if (.not. is_num) then; istart = ii; iend = ii; return; end if
     istart = ii
     do ii = min(iend + 1, nn), nn
        tok = string(ii:ii)
        if (tok == '.') exit
        iend = ii
     end do
  end subroutine
  subroutine parse(self, string)
     type(acc_t), intent(out) :: self
     character(len=*), intent(in) :: string
     integer :: istart, iend, nn, stat, num(8)
     logical :: is_num
     nn = 0; iend = 0; istart = 0; is_num = .false.
     do while (iend < len(string))
        call step(string, istart, iend, is_num)
        if (is_num) then
           nn = nn + 1
           read(string(istart:iend), *, iostat=stat) num(nn)
        end if
     end do
     self%num = num(:nn)
  end subroutine
end module

module m_second
  implicit none
  type :: lexer_t
     integer :: pos = 0
  end type
contains
  ! Same bare name `step`, but first arg is NON-character.
  subroutine step(lexer, token)
     type(lexer_t), intent(inout) :: lexer
     integer, intent(out) :: token
     lexer%pos = lexer%pos + 1
     token = lexer%pos
  end subroutine
end module

program p
  use m_first, only: acc_t, parse
  implicit none
  type(acc_t) :: v
  call parse(v, '1.22.333')
  write(*, '(a,i0,a,i0,1x,i0,1x,i0)') 'n=', size(v%num), ' vals=', v%num(1), v%num(2), v%num(3)
  if (size(v%num) /= 3) error stop 1
  if (v%num(1) /= 1)   error stop 2
  if (v%num(2) /= 22)  error stop 3
  if (v%num(3) /= 333) error stop 4
end program p
! CHECK: n=3 vals=1 22 333
