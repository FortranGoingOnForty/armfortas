! CHECK: arg= 8 32
! CHECK: comp= 8 32 USE
! CHECK: bare= 8 32 RST
! CHECK: ok
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program ar9_char_ac_typespec
  implicit none

  type :: holder_t
    character(len=:), allocatable :: values(:)
  end type holder_t

  type(holder_t) :: h
  character(len=:), allocatable :: bare(:)

  call probe([character(len=8) :: 'AB', 'CDE'])

  h%values = [character(len=8) :: 'HO', 'USE']
  if (len(h%values) /= 8) error stop 3
  if (iachar(h%values(1)(4:4)) /= iachar(' ')) error stop 4
  if (trim(h%values(2)) /= 'USE') error stop 5
  print '(a,1x,i0,1x,i0,1x,a)', 'comp=', len(h%values), &
      iachar(h%values(1)(4:4)), trim(h%values(2))

  bare = [character(len=8) :: 'Q', 'RST']
  if (len(bare) /= 8) error stop 6
  if (iachar(bare(1)(4:4)) /= iachar(' ')) error stop 7
  if (trim(bare(2)) /= 'RST') error stop 8
  print '(a,1x,i0,1x,i0,1x,a)', 'bare=', len(bare), iachar(bare(1)(4:4)), &
      trim(bare(2))

  print '(a)', 'ok'

contains
  subroutine probe(argv)
    character(len=*), intent(in) :: argv(:)

    if (len(argv) /= 8) error stop 1
    if (iachar(argv(1)(4:4)) /= iachar(' ')) error stop 2
    print '(a,1x,i0,1x,i0)', 'arg=', len(argv), iachar(argv(1)(4:4))
  end subroutine probe
end program ar9_char_ac_typespec
