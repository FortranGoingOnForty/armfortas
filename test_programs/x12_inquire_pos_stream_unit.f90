! Regression (tomlf's read_whole_file returned nothing under armfortas):
! INQUIRE(unit=, POS=) on a stream unit must return the next file storage
! unit, 1-based (F2018 12.10.2.22). The runtime's afs_inquire_unit had no
! pos_out plumbing at all, so the specifier was silently dropped and the
! destination kept garbage; the standard file-slurp idiom
!   open(stream, position="append") ; inquire(pos=length)
!   allocate(character(length-1)) ; read(pos=1)
! sized its buffer from that garbage and every direct toml_load(file)
! user read an empty chunk.
program x12_inquire_pos_stream_unit
  implicit none
  character(len=*), parameter :: path = '/tmp/x12_inquire_pos_stream.dat'
  character(len=:), allocatable :: s
  integer :: io, stat, length
  ! write a 26-byte payload
  open(newunit=io, file=path, status='replace', access='stream', action='write')
  write(io) 'abcdefghijklmnopqrstuvwxyz'
  close(io)
  ! the slurp idiom
  open(newunit=io, file=path, status='old', access='stream', position='append', iostat=stat)
  if (stat /= 0) then
     print '(a)', 'OPEN-FAIL'
     stop 1
  end if
  length = -99
  inquire(unit=io, pos=length)
  write(*,'(a,i0)') 'pos=', length
  allocate(character(length-1) :: s)
  read(io, pos=1, iostat=stat) s(:length-1)
  write(*,'(a,i0)') 'read stat=', stat
  write(*,'(a)') 'payload=['//s//']'
  close(io, status='delete')
  print '(a)', 'DONE'
end program
! CHECK: pos=27
! CHECK: read stat=0
! CHECK: payload=[abcdefghijklmnopqrstuvwxyz]
! CHECK: DONE
