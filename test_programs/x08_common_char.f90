! x08/l06: character members of a COMMON block now lower with inline-byte
! storage, so storage association works. Previously the member got a
! pointer slot instead of inline bytes and every read came back empty.
! The block is shared between the host and a contained subroutine; the
! subroutine sees the bytes the host wrote.
! CHECK: abcd 7
! CHECK: WX 7
program x08_common_char
  implicit none
  character(4) :: tag
  integer :: cnt
  common /shared/ tag, cnt
  tag = "abcd"
  cnt = 7
  call show
  tag = "WXYZ"
  call show2
contains
  subroutine show
    character(4) :: tag
    integer :: cnt
    common /shared/ tag, cnt
    print '(A,1X,I0)', tag, cnt
  end subroutine show
  subroutine show2
    character(4) :: tag
    integer :: cnt
    common /shared/ tag, cnt
    ! Substring of the shared bytes.
    print '(A,1X,I0)', tag(1:2), cnt
  end subroutine show2
end program x08_common_char
